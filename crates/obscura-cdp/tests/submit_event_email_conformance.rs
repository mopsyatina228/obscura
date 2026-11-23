use obscura_cdp::dispatch::{dispatch, CdpContext};
use obscura_cdp::types::CdpRequest;
use serde_json::{json, Value};

async fn evaluate(expression: &str) -> Value {
    let mut ctx = CdpContext::new();
    let page_id = ctx.create_page();
    let session = "submit-event-email";
    ctx.sessions.insert(session.into(), page_id);
    let nav = dispatch(&CdpRequest {
        id: 1,
        method: "Page.navigate".into(),
        params: json!({"url":"data:text/html,<form id=f><input id=e type=email><button id=b type=submit>Continue</button></form>", "waitUntil":"load"}),
        session_id: Some(session.into()),
    }, &mut ctx).await;
    assert!(nav.error.is_none(), "navigation: {:?}", nav.error);
    let res = dispatch(&CdpRequest {
        id: 2,
        method: "Runtime.evaluate".into(),
        params: json!({"expression": expression,"returnByValue":true}),
        session_id: Some(session.into()),
    }, &mut ctx).await;
    assert!(res.error.is_none(), "evaluate: {:?}", res.error);
    let out = res.result.expect("result");
    assert!(out.get("exceptionDetails").is_none(), "JS exception: {out}");
    out["result"]["value"].clone()
}

#[tokio::test(flavor = "current_thread")]
async fn submit_event_constructor_and_real_button_activation_preserve_submitter() {
    let actual = evaluate(r#"(() => {
        const form = document.getElementById('f'), button = document.getElementById('b');
        const created = new SubmitEvent('submit', {bubbles:true,cancelable:true,submitter:button});
        const omitted = new SubmitEvent('submit');
        let observed = null;
        form.addEventListener('submit', ev => {
            observed = [ev instanceof SubmitEvent, ev instanceof Event,
                ev.submitter === button, ev.target === form, ev.bubbles, ev.cancelable];
            ev.preventDefault();
        });
        button.click();
        return {constructor:[created instanceof SubmitEvent, created.submitter === button,
            omitted.submitter === null], observed};
    })()"#).await;
    assert_eq!(actual, json!({
        "constructor":[true,true,true],
        "observed":[true,true,true,true,true,true]
    }));
}

#[tokio::test(flavor = "current_thread")]
async fn request_submit_without_button_reports_null_submitter() {
    assert_eq!(evaluate(r#"(() => {
        const form = document.getElementById('f');
        let observed = null;
        form.addEventListener('submit', ev => { observed =
            [ev instanceof SubmitEvent, ev.submitter === null]; ev.preventDefault(); });
        form.requestSubmit();
        return observed;
    })()"#).await, json!([true,true]));
}

#[tokio::test(flavor = "current_thread")]
async fn email_constraint_detects_duplicate_address_and_tracks_live_value() {
    assert_eq!(evaluate(r#"(() => {
        const e = document.getElementById('e');
        e.value = 'Scientist7lina@gmail.comScientist7lina@gmail.com';
        const bad = [e.checkValidity(), e.validity.typeMismatch, e.validity.valid];
        e.value = 'Scientist7lina@gmail.com';
        const good = [e.checkValidity(), e.validity.typeMismatch, e.validity.valid];
        e.setCustomValidity('not yet');
        const custom = [e.checkValidity(), e.validity.customError];
        e.setCustomValidity('');
        const cleared = e.checkValidity();
        e.value = 'one@example.org, two@example.org';
        const notMultiple = e.checkValidity();
        e.setAttribute('multiple', '');
        const multiple = e.checkValidity();
        return {bad,good,custom,cleared,notMultiple,multiple};
    })()"#).await, json!({
        "bad":[false,true,false], "good":[true,false,true],
        "custom":[false,true], "cleared":true, "notMultiple":false, "multiple":true
    }));
}
