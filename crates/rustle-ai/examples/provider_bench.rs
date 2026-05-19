use std::io::{self, Write};

use rustle_ai::benchmark::{self, BenchmarkProgress, ProviderBenchmarkResult};

#[tokio::main]
async fn main() {
    println!(
        "{:<14} {:<42} {:<34} {:>10} {:>14} {:>10} {:<10} Preview",
        "Backend", "Model", "Source", "Warmup", "Generation", "Chars", "Status"
    );
    io::stdout().flush().ok();

    let _results = benchmark::run_provider_benchmark_with_progress(|progress| match progress {
        BenchmarkProgress::Starting(model) => {
            eprintln!("running llama.cpp benchmark for {model}...");
        }
        BenchmarkProgress::Finished(result) => {
            print_result(result);
            io::stdout().flush().ok();
        }
    })
    .await;
}

fn print_result(result: &ProviderBenchmarkResult) {
    println!(
        "{:<14?} {:<42} {:<34} {:>7}ms {:>11}ms {:>10} {:<10} {}",
        result.provider,
        result.model,
        result.model_source,
        result.warmup_ms,
        result.generation_ms,
        result.output_chars,
        result.status,
        result.preview
    );
}
