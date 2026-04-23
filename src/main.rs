#![forbid(unsafe_code)]
#![cfg_attr(
    not(test),
    deny(
        clippy::expect_used,
        clippy::panic,
        clippy::todo,
        clippy::unimplemented,
        clippy::unreachable,
        clippy::unwrap_used
    )
)]

#[tokio::main]
async fn main() {
    let code = salus::main_entry(std::env::args_os()).await;
    std::process::exit(code);
}
