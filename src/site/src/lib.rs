//! The pigeon.dev site: its pages, in pigeon's design system. Built for `wasm32-unknown-unknown`.

#[cfg(not(target_arch = "wasm32"))]
compile_error!("pigeon-site builds for wasm32 only: cargo build --target wasm32-unknown-unknown");

mod architecture;

use pigeon_capabilities::design;
use worker::{Context, Env, Headers, Method, Request, Response, Result, event};

#[event(fetch)]
async fn fetch(req: Request, _env: Env, _ctx: Context) -> Result<Response> {
    match (req.method(), req.path().as_str()) {
        (Method::Get, "/") => html(&architecture::page()),
        _ => Response::error("not found", 404),
    }
}

fn html(body: &str) -> Result<Response> {
    let headers = Headers::new();
    headers.set("Content-Type", "text/html; charset=utf-8")?;
    headers.set("Content-Security-Policy", design::CSP)?;
    headers.set("X-Content-Type-Options", "nosniff")?;
    headers.set("Referrer-Policy", "no-referrer")?;
    Ok(Response::ok(body)?.with_headers(headers))
}
