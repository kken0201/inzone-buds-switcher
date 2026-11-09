mod hid;
mod probe;

use anyhow::{Result, bail};

fn main() -> Result<()> {
    let args: Vec<String> = std::env::args().skip(1).collect();
    match args.first().map(String::as_str) {
        Some("probe") => probe::run(&args[1..]),
        _ => {
            eprintln!("usage: inzone-buds-switcher probe [--battery <秒>] [--vendor-only]");
            bail!("サブコマンドを指定してください");
        }
    }
}
