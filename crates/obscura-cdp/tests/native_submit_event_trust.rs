use obscura_cdp::dispatch::{dispatch, CdpContext};
use obscura_cdp::types::CdpRequest;
use serde_json::{json, Value};

async fn cdp(ctx: &mut CdpContext, id: u64, method: &str, params: Value) -> Value {
    let response = dispatch(
        &CdpRequest {
            id,
            method: method.to_owned(),
            params,
            session_id: Some("native-submit-trust".to_owned()),
        },
        ctx,
    ).await;
    assert!(response.error.is_none(), "{method}: {:?}", response.error);
    response.result.unwrap_or_else(|| json!({}))
}

async fn setup() -> CdpContext {
    let mut ctx = CdpContext::new();
    let page_id = ctx.create_page();
    ctx.sessions.insert("native-submit-trust".into(), page_id);
    cdp(
        &mut ctx, 1, "Page.navigate",
        json!({
            "url":"data:text/html,<html><body><form id=f><input name=code value=123456><button id=b type=submit name=intent value=validate>Continue</button></form></body></html>",
            "waitUntil":"load"
        })
    ).await;
    ctx
}

async fn eval(ctx: &mut CdpContext, id: u64, expression: &str) -> Value {
    let response = cdp(ctx, id, "Runtime.evaluate", json!({
        "expression": expression, "returnByValue": true
    })).await;
    assert!(response.get("exceptionDetails").is_none(), "JS exception: {response}");
    response["result"]["value"].clone()
}

#[tokio::test(flavor = "current_thread")]
async fn browser_generated_submit_events_are_trusted_but_script_events_are_not() {
    let mut ctx = setup().await;
    let result = eval(&mut ctx, 2, r#"(() => {
        const form = document.getElementById('f');
        const button = document.getElementById('b');
        const observations = [];
        form.addEventListener('submit', e => {
            observations.push({
                trusted: e.isTrusted,
                submitter: e.submitter === button,
                valid: e instanceof SubmitEvent,
                intent: new FormData(form, e.submitter).get('intent')
            });
            e.preventDefault();
        });
        const scripted = new SubmitEvent('submit', {submitter:button});
        const initial = scripted.isTrusted;
        form.requestSubmit(button);
        let clickTrusted = null;
        button.addEventListener('click', e => clickTrusted = e.isTrusted);
        button.click();
        return {initial, clickTrusted, observations};
    })()"#).await;
    assert_eq!(result, json!({
        "initial": false,
        "clickTrusted": false,
        "observations": [
            {"trusted":true,"submitter":true,"valid":true,"intent":"validate"},
            {"trusted":true,"submitter":true,"valid":true,"intent":"validate"}
        ]
    }));
}

#[cfg(feature = "render")]
#[tokio::test(flavor = "current_thread")]
async fn cdp_pointer_click_preserves_trustedness_through_default_submit() {
    let mut ctx = setup().await;
    eval(&mut ctx, 3, r#"(() => {
        const form = document.getElementById('f');
        const button = document.getElementById('b');
        window.__submitTrace = [];
        button.addEventListener('click', e => {
            window.__submitTrace.push(['click',e.isTrusted]);
        });
        form.addEventListener('submit', e => {
            window.__submitTrace.push(['submit',e.isTrusted,e.submitter===button]);
            e.preventDefault();
        });
    })()"#).await;
    let coords = eval(&mut ctx, 4, r#"(() => {
        const r=document.getElementById('b').getBoundingClientRect();
        return {x:r.left+r.width/2,y:r.top+r.height/2};
    })()"#).await;
    let x = coords["x"].as_f64().expect("x");
    let y = coords["y"].as_f64().expect("y");
    for (id, kind) in [(5,"mouseMoved"),(6,"mousePressed"),(7,"mouseReleased")] {
        cdp(&mut ctx, id, "Input.dispatchMouseEvent", json!({
            "type":kind,"x":x,"y":y,"button":"left","clickCount":1
        })).await;
    }
    let events = eval(&mut ctx, 8, "window.__submitTrace").await;
    assert_eq!(events, json!([["click",true],["submit",true,true]]));
}