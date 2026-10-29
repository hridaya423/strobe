use std::process::ExitCode;

use anyhow::Result;
use vrf_blink::Ui;

pub type Case = (&'static str, fn(&Ui) -> Result<()>);

pub fn run(cases: &[Case]) -> ExitCode {
    let filters: Vec<String> = std::env::args()
        .skip(1)
        .filter(|arg| !arg.starts_with('-'))
        .collect();
    let selected: Vec<Case> = cases
        .iter()
        .copied()
        .filter(|(name, _)| filters.is_empty() || filters.iter().any(|f| name.contains(f.as_str())))
        .collect();
    if selected.is_empty() {
        println!("running 0 tests\n\ntest result: ok. 0 passed; 0 failed");
        return ExitCode::SUCCESS;
    }
    let results = vrf_blink::run(move |ui| {
        println!("running {} tests", selected.len());
        selected
            .into_iter()
            .map(|(name, case)| {
                let result = case(&ui);
                match &result {
                    Ok(()) => println!("test {name} ... ok"),
                    Err(error) => println!("test {name} ... FAILED\n{error:#}"),
                }
                result.is_ok()
            })
            .collect::<Vec<_>>()
    });
    let passed = results.iter().filter(|ok| **ok).count();
    let failed = results.len() - passed;
    let verdict = if failed == 0 { "ok" } else { "FAILED" };
    println!("\ntest result: {verdict}. {passed} passed; {failed} failed");
    if failed == 0 {
        ExitCode::SUCCESS
    } else {
        ExitCode::FAILURE
    }
}
