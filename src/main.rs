#![forbid(unsafe_code)]

#[tokio::main]
async fn main() {
    let code = salus::main_entry(std::env::args_os()).await;
    std::process::exit(code);
}
