mod bundle;
mod host;
mod mac;

use anyhow::Result;

fn main() -> Result<()> {
    let args: Vec<String> = std::env::args().skip(1).collect();
    host::run(move |ui| -> Result<()> {
        match args.first().map(String::as_str) {
            Some("probe") => probe(&ui),
            other => anyhow::bail!("unknown command {other:?}"),
        }
    })
}

fn probe(ui: &host::Ui) -> Result<()> {
    let page = format!("file://{}/pages/probe.html", env!("CARGO_MANIFEST_DIR"));
    let mut view = host::View::open(ui, &page, 640, 360, false)?;
    let result = view.evaluate("probe()")?;
    println!("{}", serde_json::to_string_pretty(&result)?);
    view.close()
}
