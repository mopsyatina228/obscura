use obscura_cdp::dispatch::{dispatch, CdpContext};
use obscura_cdp::types::CdpRequest;
use serde_json::{json, Value};

async fn evaluate(code: &str) -> Value {
    let mut ctx = CdpContext::new();
    let page_id = ctx.create_page();
    ctx.sessions.insert("response-formdata".to_owned(), page_id);
    let nav = dispatch(&CdpRequest {
        id: 1,
        method: "Page.navigate".into(),
        params: json!({"url":"data:text/html,<html><body>FormData response tests</body></html>","waitUntil":"load"}),
        session_id: Some("response-formdata".into()),
    }, &mut ctx).await;
    assert!(nav.error.is_none(), "navigation: {:?}", nav.error);
    let response = dispatch(&CdpRequest {
        id: 2,
        method: "Runtime.evaluate".into(),
        params: json!({"expression":code, "awaitPromise":true,"returnByValue":true}),
        session_id: Some("response-formdata".into()),
    }, &mut ctx).await;
    assert!(response.error.is_none(), "evaluation: {:?}", response.error);
    let result = response.result.expect("Runtime.evaluate result");
    assert!(result.get("exceptionDetails").is_none(), "JS exception: {result}");
    result["result"]["value"].clone()
}

#[tokio::test(flavor = "current_thread")]
async fn urlencoded_response_formdata_decodes_and_consumes_once() {
    let got = evaluate(r#"(async()=>{
        const response = new Response('a=hello+world&a=two&b=%E2%9C%93', {
            headers:{'content-type':'application/x-www-form-urlencoded;charset=UTF-8'}
        });
        const data = await response.formData();
        let twice = false;
        try { await response.formData(); } catch (e) { twice = e instanceof TypeError; }
        return {entries:[...data.entries()],bodyUsed:response.bodyUsed,twice};
    })()"#).await;
    assert_eq!(got,json!({"entries":[["a","hello world"],["a","two"],["b","✓"]],"bodyUsed":true,"twice":true}));
}

#[tokio::test(flavor = "current_thread")]
async fn multipart_response_formdata_preserves_file_data() {
    let got = evaluate(r#"(async()=>{
        const boundary='obscura-boundary';
        const body='--'+boundary+'\r\nContent-Disposition: form-data; name="title"\r\n\r\nExample'
            +'\r\n--'+boundary+'\r\nContent-Disposition: form-data; name="upload"; filename="hello.txt"'
            +'\r\nContent-Type: text/plain\r\n\r\nhello\r\n--'+boundary+'--\r\n';
        const response = new Response(new TextEncoder().encode(body), {
            headers:{'content-type':'multipart/form-data; boundary='+boundary}
        });
        const data = await response.formData();
        const file = data.get('upload');
        return {title:data.get('title'),filename:file.name,fileType:file.type,size:file.size,
            content:await file.text(),used:response.bodyUsed};
    })()"#).await;
    assert_eq!(got,json!({"title":"Example","filename":"hello.txt","fileType":"text/plain","size":5,"content":"hello","used":true}));
}

#[tokio::test(flavor = "current_thread")]
async fn response_formdata_rejects_unknown_content_type_and_clones_are_independent() {
    let got = evaluate(r#"(async()=>{
        const jsonResponse = new Response('{}',{headers:{'content-type':'application/json'}});
        let badType = null;
        try { await jsonResponse.formData(); } catch(e) { badType=e.name; }
        const r = new Response('x=1',{headers:{'content-type':'application/x-www-form-urlencoded'}});
        const copy=r.clone();
        const first=await r.formData(),second=await copy.formData();
        return {badType,badUsed:jsonResponse.bodyUsed,original:first.get('x'),
            cloned:second.get('x'),used:r.bodyUsed,cloneUsed:copy.bodyUsed};
    })()"#).await;
    assert_eq!(got,json!({"badType":"TypeError","badUsed":true,"original":"1","cloned":"1","used":true,"cloneUsed":true}));
}

#[tokio::test(flavor = "current_thread")]
async fn response_formdata_infers_multipart_mime_and_round_trips_entries() {
    let got = evaluate(r#"(async()=>{
        const data = new FormData();
        data.append('title','Hello ✓');
        data.append('title','Second');
        data.append('file',new File(['abc'],'note.txt',{type:'text/plain'}));
        const response = new Response(data);
        const ct = response.headers.get('content-type');
        const clone = response.clone();
        const decoded = await response.formData();
        const file = decoded.get('file');
        const cloneData = await clone.formData();
        return {type:ct && ct.startsWith('multipart/form-data; boundary='),
            boundary:ct && ct.split('boundary=')[1],
            boundaryMatchesClone:clone.headers.get('content-type')===ct,
            titles:decoded.getAll('title'),fileName:file.name,fileType:file.type,
            fileText:await file.text(),cloneTitle:cloneData.get('title'),
            consumed:response.bodyUsed,cloneConsumed:clone.bodyUsed};
    })()"#).await;
    assert_eq!(got["type"],json!(true));
    assert_eq!(got["boundaryMatchesClone"],json!(true));
    assert!(got["boundary"].as_str().unwrap().starts_with("----WebKitFormBoundary"));
    assert_eq!(got["titles"],json!(["Hello ✓","Second"]));
    assert_eq!(got["fileName"],json!("note.txt"));
    assert_eq!(got["fileType"],json!("text/plain"));
    assert_eq!(got["fileText"],json!("abc"));
    assert_eq!(got["cloneTitle"],json!("Hello ✓"));
    assert_eq!(got["consumed"],json!(true));
    assert_eq!(got["cloneConsumed"],json!(true));
}

#[tokio::test(flavor = "current_thread")]
async fn response_bodyinit_infers_other_mime_types_without_overriding_explicit_header() {
    let got = evaluate(r#"(async()=>{
        const form = new URLSearchParams('name=hello+world&name=again');
        const url = new Response(form);
        const parsed = await url.formData();
        const blob = new Response(new Blob(['ok'],{type:'text/plain'}));
        const string = new Response('hello');
        const bytes = new Response(new TextEncoder().encode('abc'));
        const empty = new Response(null);
        const explicit = new Response(form,{headers:{'Content-Type':'application/custom'}});
        const formData = new FormData();formData.append('a','1');
        const multipartExplicit = new Response(formData,{headers:{'content-type':'custom/value'}});
        return {
            urlType:url.headers.get('content-type'),urlValues:parsed.getAll('name'),
            blobType:blob.headers.get('content-type'),blobBody:await blob.text(),
            stringType:string.headers.get('content-type'),
            bytesType:bytes.headers.get('content-type'),
            emptyType:empty.headers.get('content-type'),
            explicitType:explicit.headers.get('content-type'),
            multipartExplicitType:multipartExplicit.headers.get('content-type')
        };
    })()"#).await;
    assert_eq!(got,json!({
        "urlType":"application/x-www-form-urlencoded;charset=UTF-8",
        "urlValues":["hello world","again"],
        "blobType":"text/plain","blobBody":"ok",
        "stringType":"text/plain;charset=UTF-8","bytesType":null,"emptyType":null,
        "explicitType":"application/custom","multipartExplicitType":"custom/value"
    }));
}
