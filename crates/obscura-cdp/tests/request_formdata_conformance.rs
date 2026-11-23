use obscura_cdp::dispatch::{dispatch, CdpContext};
use obscura_cdp::types::CdpRequest;
use serde_json::{json, Value};

async fn evaluate(expression: &str) -> Value {
    let mut ctx = CdpContext::new();
    let page_id = ctx.create_page();
    ctx.sessions.insert("request-formdata".into(), page_id);
    let nav = dispatch(&CdpRequest {
        id: 1,
        method: "Page.navigate".into(),
        params: json!({"url":"data:text/html,<form id=f><input name=code value=123456><button id=b name=intent value=continue>Continue</button></form>", "waitUntil":"load"}),
        session_id: Some("request-formdata".into()),
    }, &mut ctx).await;
    assert!(nav.error.is_none(), "navigation: {:?}", nav.error);
    let res = dispatch(&CdpRequest {
        id: 2,
        method: "Runtime.evaluate".into(),
        params: json!({"expression": expression, "returnByValue":true, "awaitPromise":true}),
        session_id: Some("request-formdata".into()),
    }, &mut ctx).await;
    assert!(res.error.is_none(), "evaluate: {:?}", res.error);
    let value = res.result.expect("result");
    assert!(value.get("exceptionDetails").is_none(), "JS exception: {value}");
    value["result"]["value"].clone()
}

#[tokio::test(flavor = "current_thread")]
async fn request_formdata_parses_form_and_preserves_clicked_submitter() {
    let got=evaluate(r#"(async()=>{
      const f=document.getElementById('f'), b=document.getElementById('b');
      const body=new FormData(f,b),req=new Request('https://example.test/action',{method:'POST',body});
      const before=req.bodyUsed,parsed=await req.formData(),after=req.bodyUsed;
      let second=null;try{await req.formData()}catch(e){second=e.name}
      let cloneError=null;try{req.clone()}catch(e){cloneError=e.name}
      return {before,after,fields:[...parsed.entries()],second,cloneError,
              originalFields:[...body.entries()]};
    })()"#).await;
    assert_eq!(got,json!({
      "before":false,"after":true,
      "fields":[["code","123456"],["intent","continue"]],
      "second":"TypeError", "cloneError":"TypeError",
      "originalFields":[["code","123456"],["intent","continue"]]
    }));
}

#[tokio::test(flavor = "current_thread")]
async fn request_formdata_parses_urlsearchparams_without_fetch() {
    let got=evaluate(r#"(async()=>{
      const params=new URLSearchParams('name=Alice+Bob&intent=resend');
      const req=new Request('https://example.test/action',{method:'POST',body:params});
      const parsed=await req.formData();
      return {entries:[...parsed.entries()],used:req.bodyUsed};
    })()"#).await;
    assert_eq!(got,json!({"entries":[["name","Alice Bob"],["intent","resend"]],"used":true}));
}

#[tokio::test(flavor = "current_thread")]
async fn request_formdata_unsupported_body_fails_without_sending_http() {
    let got=evaluate(r#"(async()=>{
      const req=new Request('https://example.test/action',{method:'POST',body:'raw',headers:{'content-type':'text/plain'}});
      let result=null;
      try{await req.formData()}catch(e){result=e.name}
      return {result,used:req.bodyUsed};
    })()"#).await;
    assert_eq!(got,json!({"result":"TypeError","used":true}));
}
