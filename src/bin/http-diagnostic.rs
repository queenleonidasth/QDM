//! Owned public-origin diagnostic. Does not open QDM history or publish files.
//! Build with `cargo build --release --locked --bin http-diagnostic` so the
//! accepted QDM.exe is left untouched.
use reqwest::{header::{ACCEPT_ENCODING, RANGE}, Client};
use serde_json::{json, Value};
use std::{error::Error, time::{Duration, Instant}};
use tokio::task::JoinSet;

fn option(args: &[String], name: &str, fallback: &str) -> String {
    args.windows(2).find(|pair| pair[0] == name).map(|pair| pair[1].clone()).unwrap_or_else(|| fallback.into())
}
fn chain(error: &(dyn Error + 'static)) -> Vec<String> {
    let mut causes = Vec::new();
    let mut current = Some(error);
    while let Some(error) = current {
        causes.push(error.to_string());
        if causes.len() >= 12 { break; }
        current = error.source();
    }
    causes
}
fn output(value: Value) { println!("{}", value); }

async fn transfer(client: Client, url: String, worker: usize, start: u64, end: u64) -> Value {
    let started = Instant::now();
    let request = client.get(&url).header(ACCEPT_ENCODING, "identity")
        .header(RANGE, format!("bytes={start}-{end}"));
    let mut response = match tokio::time::timeout(Duration::from_secs(30), request.send()).await {
        Ok(Ok(response)) => response,
        Ok(Err(error)) => return json!({"worker":worker,"stage":"send","ok":false,"elapsedMs":started.elapsed().as_millis(),"chain":chain(&error),"debug":format!("{error:?}"),"timeout":error.is_timeout(),"connect":error.is_connect()}),
        Err(_) => return json!({"worker":worker,"stage":"send","ok":false,"elapsedMs":started.elapsed().as_millis(),"error":"Header timeout at30s"}),
    };
    let status = response.status().as_u16();
    let version = format!("{:?}", response.version());
    let remote = response.remote_addr().map(|value| value.to_string());
    let content_range = response.headers().get("content-range").and_then(|value| value.to_str().ok()).map(str::to_owned);
    let content_length = response.content_length();
    let header_ms = started.elapsed().as_millis();
    output(json!({"event":"headers","worker":worker,"status":status,"version":version,"remote":remote,"headerMs":header_ms,"contentRange":content_range,"contentLength":content_length}));
    if status != 206 {
        return json!({"worker":worker,"stage":"status","ok":false,"status":status,"elapsedMs":started.elapsed().as_millis(),"remote":remote,"contentRange":content_range});
    }
    let mut received = 0u64;
    let mut first_byte_ms = None;
    loop {
        match tokio::time::timeout(Duration::from_secs(45), response.chunk()).await {
            Ok(Ok(Some(chunk))) => {
                if first_byte_ms.is_none() { first_byte_ms = Some(started.elapsed().as_millis()); }
                received += chunk.len() as u64;
                if received > end - start + 1 {
                    return json!({"worker":worker,"stage":"body","ok":false,"error":"Response exceeded requested range","bytes":received,"elapsedMs":started.elapsed().as_millis()});
                }
            }
            Ok(Ok(None)) => break,
            Ok(Err(error)) => return json!({"worker":worker,"stage":"body","ok":false,"bytes":received,"firstByteMs":first_byte_ms,"elapsedMs":started.elapsed().as_millis(),"remote":remote,"chain":chain(&error),"debug":format!("{error:?}"),"timeout":error.is_timeout()}),
            Err(_) => return json!({"worker":worker,"stage":"body","ok":false,"bytes":received,"firstByteMs":first_byte_ms,"elapsedMs":started.elapsed().as_millis(),"error":"Body idle timeout at45s"}),
        }
    }
    json!({"worker":worker,"ok":received == end-start+1,"bytes":received,"expectedBytes":end-start+1,"headerMs":header_ms,"firstByteMs":first_byte_ms,"elapsedMs":started.elapsed().as_millis(),"remote":remote,"version":version,"contentRange":content_range})
}

fn main() {
    let args: Vec<String> = std::env::args().collect();
    let url = option(&args,"--url","");
    if !url.starts_with("https://") && !url.starts_with("http://") { eprintln!("Pass --url HTTP(S)_URL [--connections1|4|32] [--range-bytes1048576] [--no-proxy] [--http-versionauto] [--ipv4]");std::process::exit(2); }
    let connections = option(&args,"--connections","32").parse::<usize>().unwrap().clamp(1,64);
    let range_bytes = option(&args,"--range-bytes","1048576").parse::<u64>().unwrap().clamp(1,4*1024*1024);
    let no_proxy = args.iter().any(|arg| arg=="--no-proxy");
    let ipv4 = args.iter().any(|arg| arg=="--ipv4");
    let http_version = option(&args,"--http-version","1");
    let user_agent = option(&args,"--user-agent","QDM/0.2.0");
    let runtime = tokio::runtime::Builder::new_multi_thread().worker_threads(2).max_blocking_threads(4)
        .thread_name("qdm-net-diagnostic").enable_all().build().unwrap();
    let failed = runtime.block_on(async {
        let mut builder = Client::builder().connect_timeout(Duration::from_secs(20))
            .pool_idle_timeout(Duration::from_secs(30)).pool_max_idle_per_host(64)
            .tcp_keepalive(Duration::from_secs(30)).user_agent(user_agent.clone()).referer(false)
            .no_gzip().no_brotli().no_deflate().no_zstd();
        if http_version == "1" { builder = builder.http1_only(); }
        if no_proxy { builder = builder.no_proxy(); }
        let mut resolved = Vec::new();
        if ipv4 {
            let parsed = url::Url::parse(&url).unwrap();
            let host = parsed.host_str().unwrap();
            resolved = tokio::net::lookup_host((host,parsed.port_or_known_default().unwrap())).await.unwrap().filter(|address| address.is_ipv4()).collect();
            if resolved.is_empty() { panic!("No IPv4 address resolved"); }
            builder = builder.resolve_to_addrs(host,&resolved);
        }
        let client = builder.build().unwrap();
        output(json!({"event":"configuration","url":url,"connections":connections,"rangeBytes":range_bytes,"httpVersion":http_version,"noProxy":no_proxy,"ipv4":ipv4,"resolved":resolved,"userAgent":user_agent,"tls":"native-tls with certificate and hostname validation enabled","runtimeWorkers":2,"blockingThreadsMax":4}));
        let probe = transfer(client.clone(),url.clone(),0,0,0).await;
        let total = probe["contentRange"].as_str().and_then(|value| value.rsplit('/').next()).and_then(|value| value.parse::<u64>().ok());
        output(json!({"event":"probe","result":probe}));
        let Some(total) = total else { output(json!({"event":"finished","ok":false,"error":"No valid total from initial probe"}));return true; };
        let started = Instant::now();
        let mut tasks = JoinSet::new();
        for index in 0..connections {
            let start = (index as u64 * range_bytes) % total;
            let end = (start+range_bytes-1).min(total-1);
            tasks.spawn(transfer(client.clone(),url.clone(),index+1,start,end));
        }
        let mut failures = 0;
        while let Some(result) = tasks.join_next().await {
            match result { Ok(value) => { if value["ok"] != true { failures += 1; } output(json!({"event":"worker","result":value})); }, Err(error) => { failures += 1; output(json!({"event":"workerPanic","error":error.to_string()})); } }
        }
        output(json!({"event":"finished","connections":connections,"failed":failures,"ok":failures==0,"elapsedMs":started.elapsed().as_millis()}));
        failures > 0
    });
    if failed { std::process::exit(1); }
}
