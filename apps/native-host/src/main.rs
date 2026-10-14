//! Browser-spawned bridge. stdout is exclusively native-messaging frames.
use okytes_browser_bridge::{
    forward,
    protocol::{Request, Response},
    read_frame, write_frame,
};
mod install;

#[tokio::main]
async fn main() {
    if std::env::args().nth(1).as_deref() == Some("install") {
        if let Err(error) = install::run() {
            eprintln!("Host installation failed: {error}");
            std::process::exit(1);
        }
        return;
    }
    let directory = match install::profile() {
        Ok(path) => path,
        Err(_) => return,
    };
    // Native host permits only the IDs recorded by its installer. Browser-provided
    // arguments are checked as defence in depth; manifest allowlists are primary.
    if !install::caller_allowed(&directory) {
        return;
    }
    let mut input = tokio::io::stdin();
    let mut output = tokio::io::stdout();
    let mut count = 0u32;
    let mut started = std::time::Instant::now();
    loop {
        let request: Request = match read_frame(&mut input).await {
            Ok(Some(value)) => value,
            _ => break,
        };
        if started.elapsed().as_secs() >= 60 {
            started = std::time::Instant::now();
            count = 0;
        }
        count += 1;
        let response = if count > 120 {
            Response::reply(request.id, Err("rate_limited".into()))
        } else if request.validate().is_err() {
            Response::reply(request.id, Err("invalid_request".into()))
        } else {
            let id = request.id.clone();
            match forward(&directory, request).await {
                Ok(response) => response,
                Err(_) => Response::reply(id, Err("app_unavailable".into())),
            }
        };
        if write_frame(&mut output, &response).await.is_err() {
            break;
        }
    }
}
