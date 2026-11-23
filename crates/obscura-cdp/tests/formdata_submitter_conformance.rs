use obscura_cdp::dispatch::{dispatch, CdpContext};
use obscura_cdp::types::CdpRequest;
use serde_json::{json, Value};

const HTML: &str = r#"<form id=f><input id=code name=code value=123456><button id=continue type=submit name=intent value=continue>Continue</button><button id=resend type=submit name=intent value=resend>Resend email</button><button id=other type=button name=intent value=bad>Not a submitter</button></form><form id=g><button id=wrong type=submit>Wrong form</button></form>"#;

async fn evaluate(expression: &str) -> Value {
    let mut ctx = CdpContext::new();
    let page_id = ctx.create_page();
    ctx.sessions.insert("formdata-submit".into(), page_id);
    let response = dispatch(
        &CdpRequest {
            id: 1,
            method: "Page.navigate".into(),
            params: json!({"url":format!("data:text/html,{HTML}"),"waitUntil":"load"}),
            session_id: Some("formdata-submit".into()),
        },
        &mut ctx,
    ).await;
    assert!(response.error.is_none(), "navigation: {:?}", response.error);
    let response = dispatch(
        &CdpRequest {
            id: 2,
            method: "Runtime.evaluate".into(),
            params: json!({"expression":expression,"returnByValue":true}),
            session_id: Some("formdata-submit".into()),
        },
        &mut ctx,
    ).await;
    assert!(response.error.is_none(), "evaluation: {:?}", response.error);
    let value = response.result.expect("result");
    assert!(value.get("exceptionDetails").is_none(), "{value}");
    value["result"]["value"].clone()
}

#[tokio::test(flavor = "current_thread")]
async fn react_router_formdata_includes_only_clicked_intent_submitter() {
    let got = evaluate(r#"(() => {
      const f=document.getElementById('f'),cont=document.getElementById('continue'),resend=document.getElementById('resend');
      return {
        none:Array.from(new FormData(f).entries()),
        continue:Array.from(new FormData(f,cont).entries()),
        resend:Array.from(new FormData(f,resend).entries()),
      };
    })()"#).await;
    assert_eq!(got,json!({
        "none":[["code","123456"]],
        "continue":[["code","123456"],["intent","continue"]],
        "resend":[["code","123456"],["intent","resend"]]
    }));
}

#[tokio::test(flavor = "current_thread")]
async fn formdata_submitter_rejects_non_submitter_and_wrong_form() {
    let got=evaluate(r#"(() => {
      const f=document.getElementById('f');
      let bad=null,wrong=null;
      try {new FormData(f,document.getElementById('other'))}
      catch(e) {bad=e.name}
      try {new FormData(f,document.getElementById('wrong'))}
      catch(e) {wrong=e.name}
      return {bad,wrong}
    })()"#).await;
    assert_eq!(got,json!({"bad":"TypeError","wrong":"NotFoundError"}));
}

#[tokio::test(flavor = "current_thread")]
async fn formdata_uses_live_submitter_value_and_does_not_mutate_form() {
    let got=evaluate(r#"(() => {
      const f=document.getElementById('f'),b=document.getElementById('resend');
      b.value='resend-updated';
      const entries=Array.from(new FormData(f,b).entries());
      return {entries,formControls:f.elements.length,buttonName:b.name};
    })()"#).await;
    assert_eq!(got,json!({
        "entries":[["code","123456"],["intent","resend-updated"]],
        "formControls":4, "buttonName":"intent"
    }));
}
