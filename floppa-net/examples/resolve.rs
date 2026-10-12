//! Read-only endpoint diagnostics: cargo run -p floppa-net --example resolve -- HOST
#[tokio::main]
async fn main() {
    let host = std::env::args().nth(1).expect("usage: resolve HOST");
    let resolver = floppa_net::Resolver::new(None);
    let mut answers = resolver.candidates(&host);
    let start = std::time::Instant::now();
    loop {
        let ips = answers.borrow_and_update().clone();
        if !ips.is_empty() {
            println!("{:?}: {ips:?}", start.elapsed());
        }
        if answers.changed().await.is_err() {
            break;
        }
    }
    if answers.borrow().is_empty() {
        eprintln!("no usable addresses");
        std::process::exit(1);
    }
}
