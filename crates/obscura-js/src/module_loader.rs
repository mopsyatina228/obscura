use std::cell::RefCell;
use std::pin::Pin;
use std::rc::{Rc, Weak};
use std::sync::Arc;

use base64::{engine::general_purpose::STANDARD as BASE64, Engine as _};

use deno_core::error::ModuleLoaderError;
use deno_core::ModuleLoadOptions;
use deno_core::ModuleLoadReferrer;
use deno_core::ModuleLoadResponse;
use deno_core::ModuleLoader;
use deno_core::ModuleSource;
use deno_core::ModuleSourceCode;
use deno_core::ModuleSpecifier;

use deno_error::JsErrorBox;

use crate::import_map::ImportMap;
use crate::ops::{
    record_network_completion, FetchNetworkGuard, JsNetworkEvent, ObscuraState,
    StoredNetworkResponseBody,
};

/// Observable network activity for ES-module graphs.
///
/// deno_core keeps dynamic-import state inside its private module map. The
/// browser lifecycle still needs to distinguish a genuinely idle page from a
/// graph whose fetch future is being advanced in short event-loop slices. A
/// loader-owned counter provides that signal without treating unrelated
/// fetch/XHR analytics as render-blocking work.
#[derive(Debug, Default)]
pub(crate) struct ModuleLoadActivity {
    pending: std::sync::atomic::AtomicUsize,
    last_activity: std::sync::Mutex<Option<std::time::Instant>>,
}

impl ModuleLoadActivity {
    fn begin(self: &Arc<Self>) -> ModuleLoadGuard {
        self.pending
            .fetch_add(1, std::sync::atomic::Ordering::Relaxed);
        *self
            .last_activity
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner()) = Some(std::time::Instant::now());
        ModuleLoadGuard(self.clone())
    }

    pub(crate) fn is_pending_or_recent(&self, grace: std::time::Duration) -> bool {
        if self.pending.load(std::sync::atomic::Ordering::Relaxed) != 0 {
            return true;
        }
        self.last_activity
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
            .is_some_and(|last| last.elapsed() <= grace)
    }
}

struct ModuleLoadGuard(Arc<ModuleLoadActivity>);

impl Drop for ModuleLoadGuard {
    fn drop(&mut self) {
        let previous = self
            .0
            .pending
            .fetch_sub(1, std::sync::atomic::Ordering::Relaxed);
        debug_assert!(previous > 0, "module load activity counter underflow");
        *self
            .0
            .last_activity
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner()) = Some(std::time::Instant::now());
    }
}

pub struct ObscuraModuleLoader {
    pub base_url: String,
    /// Proxy URL threaded through to every dynamic ES-module fetch (#139).
    /// `None` keeps the pre-#139 direct-connection behaviour for callers
    /// that haven't been updated.
    pub proxy_url: Option<String>,
    /// The owning page's network context. Production runtimes always install
    /// this so every module in a graph uses the same cookie jar, configured
    /// identity, redirect/security policy, interception, and callbacks as the
    /// entry module. Directly-constructed standalone loaders remain supported.
    page_state: Option<Weak<RefCell<ObscuraState>>>,
    /// Directly-constructed loaders still use Obscura's network policy and
    /// connection pool; they simply have an isolated cookie jar.
    standalone_client: Option<Arc<obscura_net::ObscuraHttpClient>>,
    import_map: Rc<RefCell<ImportMap>>,
    activity: Arc<ModuleLoadActivity>,
    /// Canonical and requested specifiers fetched into deno_core's module map.
    /// The runtime uses a cursor into this append-only list to associate a
    /// prepared root with the dependencies that its successful evaluation also
    /// evaluates.
    loaded_specifiers: Rc<RefCell<Vec<String>>>,
}

impl ObscuraModuleLoader {
    pub fn new(base_url: &str) -> Self {
        Self::with_proxy(base_url, None)
    }

    pub fn with_proxy(base_url: &str, proxy_url: Option<String>) -> Self {
        let import_map = Rc::new(RefCell::new(ImportMap::default()));
        Self::with_proxy_and_import_map(base_url, proxy_url, import_map)
    }

    fn with_proxy_and_import_map(
        base_url: &str,
        proxy_url: Option<String>,
        import_map: Rc<RefCell<ImportMap>>,
    ) -> Self {
        let standalone_client = Arc::new(obscura_net::ObscuraHttpClient::with_options(
            Arc::new(obscura_net::CookieJar::new()),
            proxy_url.as_deref(),
        ));
        ObscuraModuleLoader {
            base_url: base_url.to_string(),
            proxy_url,
            page_state: None,
            standalone_client: Some(standalone_client),
            import_map,
            activity: Arc::new(ModuleLoadActivity::default()),
            loaded_specifiers: Rc::new(RefCell::new(Vec::new())),
        }
    }

    pub(crate) fn with_page_state(
        base_url: &str,
        proxy_url: Option<String>,
        page_state: &Rc<RefCell<ObscuraState>>,
        import_map: Rc<RefCell<ImportMap>>,
    ) -> Self {
        ObscuraModuleLoader {
            base_url: base_url.to_string(),
            proxy_url,
            page_state: Some(Rc::downgrade(page_state)),
            standalone_client: None,
            import_map,
            activity: Arc::new(ModuleLoadActivity::default()),
            loaded_specifiers: Rc::new(RefCell::new(Vec::new())),
        }
    }

    pub(crate) fn activity(&self) -> Arc<ModuleLoadActivity> {
        self.activity.clone()
    }

    pub(crate) fn loaded_specifiers(&self) -> Rc<RefCell<Vec<String>>> {
        self.loaded_specifiers.clone()
    }
}

// Never treat a security/CORS denial or HTTP status as a transient failure.
fn should_retry_module_fetch(error: &obscura_net::ObscuraNetError, retries: usize) -> bool {
    retries < 2 && matches!(error, obscura_net::ObscuraNetError::Network(_))
}

fn io_err(msg: String) -> ModuleLoaderError {
    JsErrorBox::from_err(std::io::Error::new(std::io::ErrorKind::Other, msg))
}

impl ModuleLoader for ObscuraModuleLoader {
    fn resolve(
        &self,
        specifier: &str,
        referrer: &str,
        _kind: deno_core::ResolutionKind,
    ) -> Result<ModuleSpecifier, ModuleLoaderError> {
        // deno_core represents the root passed to load_side_es_module with a
        // synthetic "." referrer. A browser resolves <script type=module src>
        // as a resource URL before it starts a graph; the document import map
        // must not remap that root URL.
        if referrer == "." {
            return deno_core::resolve_import(specifier, &self.base_url)
                .map_err(|error| io_err(error.to_string()));
        }

        let base = if referrer.is_empty() || referrer.starts_with('<') || referrer == "about:blank"
        {
            &self.base_url
        } else {
            referrer
        };

        let base = ModuleSpecifier::parse(base)
            .map_err(|e| io_err(format!("Invalid module referrer {}: {}", base, e)))?;
        self.import_map
            .try_borrow_mut()
            .map_err(|_| io_err("Import map is already borrowed".to_string()))?
            .resolve(specifier, &base)
            .map_err(io_err)
    }

    fn load(
        &self,
        module_specifier: &ModuleSpecifier,
        maybe_referrer: Option<&ModuleLoadReferrer>,
        options: ModuleLoadOptions,
    ) -> ModuleLoadResponse {
        let url = module_specifier.to_string();
        // Module-graph CORS and same-origin credentials are relative to the
        // owning document, not to the importing module. The importer remains
        // the HTTP referrer for a dependency; keeping these URLs distinct
        // prevents a cross-origin parent module from gaining CDN cookies when
        // it imports a sibling on that CDN.
        let document_url =
            ModuleSpecifier::parse(&self.base_url).unwrap_or_else(|_| module_specifier.clone());
        let referrer = maybe_referrer
            .map(|referrer| referrer.specifier.clone())
            .unwrap_or_else(|| document_url.clone());
        // Capture the loader's proxy here so the async closure below owns a
        // plain Option<String> rather than borrowing &self across an `await`.
        let proxy_url = self.proxy_url.clone();
        let activity = self.activity.clone();
        let loaded_specifiers = self.loaded_specifiers.clone();
        loaded_specifiers.borrow_mut().push(url.clone());
        // Register before returning the future. The lifecycle can inspect the
        // runtime between deno_core accepting the load and first polling it.
        // Keeping the guard inside the future makes cancellation/navigation
        // decrement the count through Drop as well as success and failure.
        let is_dyn_import = options.is_dynamic_import;
        let activity_guard = is_dyn_import.then(|| activity.begin());
        let page_state = self.page_state.clone();
        let standalone_client = self.standalone_client.clone();

        ModuleLoadResponse::Async(Pin::from(Box::new(async move {
            // deno_core propagates `is_dynamic_import` to every dependency edge in
            // the recursive graph, so this excludes parser-discovered/static
            // graphs without losing descendant fetches of a lazy graph.
            let _activity_guard = activity_guard;
            tracing::debug!(
                "Loading ES module: {} (proxy: {})",
                url,
                proxy_url.as_deref().unwrap_or("direct")
            );
            // A <link rel=modulepreload> already fetched this URL (through the
            // page's fetch path, so its network event is recorded there).
            let preloaded = page_state
                .as_ref()
                .and_then(Weak::upgrade)
                .and_then(|state| {
                    state
                        .try_borrow_mut()
                        .ok()
                        .and_then(|mut state| state.module_preloads.remove(&url))
                });
            if let Some((final_url, code)) = preloaded {
                let requested = ModuleSpecifier::parse(&url)
                    .map_err(|e| io_err(format!("Invalid module URL {}: {}", url, e)))?;
                let found =
                    ModuleSpecifier::parse(&final_url).unwrap_or_else(|_| requested.clone());
                if found.as_str() != requested.as_str() {
                    loaded_specifiers.borrow_mut().push(found.to_string());
                }
                return Ok(ModuleSource::new_with_redirect(
                    deno_core::ModuleType::JavaScript,
                    ModuleSourceCode::String(code.into()),
                    &requested,
                    &found,
                    None,
                ));
            }

            // The Deno module graph can request another dependency while the
            // page state is still borrowed by the current JS operation. Defer
            // acquiring the network context until the load future is polled.
            // Yield cooperatively on bounded transient RefCell contention.
            let page_network = match page_state.as_ref() {
                Some(weak) => {
                    let mut borrow_attempts = 0usize;
                    loop {
                        let snapshot = (|| {
                            let state = weak.upgrade().ok_or_else(|| {
                                "Module loader page state was dropped".to_string()
                            })?;
                            let state = state.try_borrow().map_err(|_| {
                                "Module loader page state is already borrowed".to_string()
                            })?;
                            let client = state.http_client.clone().ok_or_else(|| {
                                "No http_client wired to module loader".to_string()
                            })?;
                            #[cfg(feature = "stealth")]
                            let stealth = state.stealth_client.clone();
                            #[cfg(not(feature = "stealth"))]
                            let stealth: Option<std::sync::Arc<()>> = None;
                            Ok((client, stealth, state.callbacks.clone()))
                        })();
                        if matches!(&snapshot, Err(e) if e == "Module loader page state is already borrowed")
                            && borrow_attempts < 3
                        {
                            borrow_attempts += 1;
                            tokio::task::yield_now().await;
                            continue;
                        }
                        break snapshot;
                    }
                }
                None => standalone_client
                    .clone()
                    .map(|client| {
                        #[cfg(feature = "stealth")]
                        let stealth = None;
                        #[cfg(not(feature = "stealth"))]
                        let stealth: Option<std::sync::Arc<()>> = None;
                        (client, stealth, None)
                    })
                    .ok_or_else(|| "No network context wired to module loader".to_string()),
            };
            match page_network {
                Ok((client, stealth, callbacks)) => {
                    let requested = ModuleSpecifier::parse(&url)
                        .map_err(|e| io_err(format!("Invalid module URL {}: {}", url, e)))?;
                    static NEXT_MODULE_REQUEST_ID: std::sync::atomic::AtomicU64 =
                        std::sync::atomic::AtomicU64::new(1);
                    let request_id = format!(
                        "module-{}",
                        NEXT_MODULE_REQUEST_ID.fetch_add(1, std::sync::atomic::Ordering::Relaxed,)
                    );
                    // Keep failed and cancelled graph fetches visible to the
                    // same Network listeners as ordinary fetch/XHR failures.
                    let mut network_guard = page_state.as_ref().map(|state| FetchNetworkGuard {
                        state: state.clone(),
                        cancel: None,
                        event: Some(JsNetworkEvent {
                            request_id: request_id.clone(),
                            intercepted: false,
                            url: url.clone(),
                            method: "GET".to_string(),
                            resource_type: "Script".to_string(),
                            status: 0,
                            response_headers: Default::default(),
                            body_size: 0,
                            timestamp: 0.0,
                            error_text: None,
                        }),
                    });
                    let request =
                        obscura_net::ResourceRequest::module_script(&document_url, &referrer);
                    // Idempotent module GET: retry only a transient transport
                    // failure, never an explicit CORS/security block, a
                    // redirect-policy rejection or an HTTP response error.
                    let mut transient_retries = 0usize;
                    let resp = loop {
                        #[cfg(feature = "stealth")]
                        let result = match &stealth {
                            Some(stealth) => {
                                stealth
                                    .fetch_resource_with_callbacks(
                                        &requested,
                                        request.clone(),
                                        callbacks.as_deref(),
                                    )
                                    .await
                            }
                            None => {
                                client
                                    .fetch_resource_with_callbacks(
                                        &requested,
                                        request.clone(),
                                        callbacks.as_deref(),
                                    )
                                    .await
                            }
                        };
                        #[cfg(not(feature = "stealth"))]
                        let result = {
                            let _ = &stealth;
                            client
                                .fetch_resource_with_callbacks(
                                    &requested,
                                    request.clone(),
                                    callbacks.as_deref(),
                                )
                                .await
                        };
                        match result {
                            Ok(resp) => break resp,
                            Err(err) if should_retry_module_fetch(&err, transient_retries) => {
                                transient_retries += 1;
                                tracing::debug!(
                                    "Transient module network error: path={} retry={}/2",
                                    requested.path(),
                                    transient_retries
                                );
                                tokio::time::sleep(std::time::Duration::from_millis(
                                    60 * transient_retries as u64,
                                ))
                                .await;
                            }
                            Err(err) => {
                                let category = match &err {
                                    obscura_net::ObscuraNetError::Network(_) => "network",
                                    obscura_net::ObscuraNetError::Cors(_) => "cors",
                                    obscura_net::ObscuraNetError::Blocked(_) => "blocked",
                                    obscura_net::ObscuraNetError::TooManyRedirects(_) => "redirect",
                                    obscura_net::ObscuraNetError::ResponseTooLarge { .. } => {
                                        "too-large"
                                    }
                                };
                                // Log class and path only; the URL's query
                                // string can contain sensitive session data.
                                tracing::warn!(
                                    "Module graph GET failed: path={} class={}",
                                    requested.path(),
                                    category
                                );
                                return Err(io_err(format!(
                                    "Failed to fetch module {}: {}",
                                    url, err
                                )));
                            }
                        }
                    };
                    // A response (including the blocked-tracker sentinel) is
                    // not a transport failure, even if evaluation later fails.
                    if let Some(guard) = &mut network_guard {
                        guard.event = None;
                    }
                    // A module response belongs to the owning page regardless
                    // of whether graph preparation or evaluation later fails.
                    // Status 0 is the transport's blocked-tracker sentinel.
                    if resp.status != 0 {
                        if let Some(state) = page_state.as_ref().and_then(Weak::upgrade) {
                            let mut state = state.try_borrow_mut().map_err(|_| {
                                io_err("Module loader page state is already borrowed".to_string())
                            })?;
                            record_network_completion(
                                &mut state,
                                JsNetworkEvent {
                                    request_id,
                                    intercepted: false,
                                    url: resp.url.to_string(),
                                    method: "GET".to_string(),
                                    resource_type: "Script".to_string(),
                                    status: resp.status,
                                    response_headers: resp.headers.clone(),
                                    body_size: resp.body.len(),
                                    timestamp: std::time::SystemTime::now()
                                        .duration_since(std::time::UNIX_EPOCH)
                                        .unwrap_or_default()
                                        .as_secs_f64(),
                                    error_text: None,
                                },
                                || match std::str::from_utf8(&resp.body) {
                                    Ok(body) => StoredNetworkResponseBody {
                                        body: body.to_string(),
                                        base64_encoded: false,
                                    },
                                    Err(_) => StoredNetworkResponseBody {
                                        body: BASE64.encode(&resp.body),
                                        base64_encoded: true,
                                    },
                                },
                            );
                        }
                    }
                    if !(200..=299).contains(&resp.status) {
                        return Err(io_err(format!(
                            "Module {} returned HTTP {}",
                            url, resp.status
                        )));
                    }
                    let found = ModuleSpecifier::parse(resp.url.as_str()).map_err(|e| {
                        io_err(format!("Invalid final module URL {}: {}", resp.url, e))
                    })?;
                    if found.as_str() != requested.as_str() {
                        loaded_specifiers.borrow_mut().push(found.to_string());
                    }
                    let code = obscura_net::decode_non_html(&resp.body, resp.content_type());
                    Ok(ModuleSource::new_with_redirect(
                        deno_core::ModuleType::JavaScript,
                        ModuleSourceCode::String(code.into()),
                        &requested,
                        &found,
                        None,
                    ))
                }
                Err(error) => Err(io_err(error)),
            }
        })))
    }
}

#[cfg(test)]
mod retry_policy_tests {
    use super::should_retry_module_fetch;
    use obscura_net::ObscuraNetError;

    #[test]
    fn retries_only_transient_module_get_errors_and_is_bounded() {
        let network = ObscuraNetError::Network("connection reset".to_string());
        assert!(should_retry_module_fetch(&network, 0));
        assert!(should_retry_module_fetch(&network, 1));
        assert!(!should_retry_module_fetch(&network, 2));
        for error in [
            ObscuraNetError::Cors("denied".to_string()),
            ObscuraNetError::Blocked("security".to_string()),
            ObscuraNetError::TooManyRedirects("loop".to_string()),
            ObscuraNetError::ResponseTooLarge {
                url: "https://example.test/bundle.js".to_string(),
                limit: 1,
            },
        ] {
            assert!(!should_retry_module_fetch(&error, 0));
        }
    }
}
