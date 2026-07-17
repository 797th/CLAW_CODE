use runtime::{compress_caveman, estimate_tokens};

#[derive(Clone, Copy)]
struct Sample {
    name: &'static str,
    input: &'static str,
    middle: &'static str,
    output: &'static str,
    required_answer_terms: &'static [&'static str],
}

const SAMPLES: &[Sample] = &[
    Sample {
        name: "auth",
        input: "Please review the authentication middleware in src/auth/token.rs and explain whether expired bearer tokens are rejected. Preserve the exact 401 error text, add regression tests, and do not change the public API.",
        middle: "Please first inspect the file src/auth/token.rs, trace the bearer-token expiry validation, preserve the exact 401 text, add the regression tests, and avoid the public API changes.",
        output: "Sure. I would be happy to help. I will inspect src/auth/token.rs, verify whether expired bearer tokens are rejected, preserve the exact 401 error text, add regression tests, and avoid changing the public API.",
        required_answer_terms: &[
            "src/auth/token.rs",
            "expired bearer tokens",
            "401",
            "regression tests",
            "public API",
        ],
    },
    Sample {
        name: "config",
        input: "Could you please update the OpenAI-compatible provider configuration so OPENAI_BASE_URL remains unchanged, timeout defaults to 30 seconds, and missing API keys fail with a clear error? Please include a focused test.",
        middle: "Please carefully plan the provider configuration change: keep the OPENAI_BASE_URL unchanged; set the timeout default to 30 seconds; make missing API keys fail clearly; include the focused regression test.",
        output: "Certainly, I can update the OpenAI-compatible provider configuration. Keep OPENAI_BASE_URL unchanged, set the timeout default to 30 seconds, fail clearly when the API key is missing, and add a focused regression test.",
        required_answer_terms: &[
            "OPENAI_BASE_URL",
            "30 seconds",
            "API key",
            "focused regression test",
        ],
    },
    Sample {
        name: "tool",
        input: "Please inspect rust/crates/runtime/src/compact.rs, find why tool-result context is retained after compaction, and propose the smallest safe fix. Do not remove error output or file paths because later tool calls depend on them.",
        middle: "Please carefully inspect rust/crates/runtime/src/compact.rs; locate the retained tool-result context; preserve the error output and file paths because later tool calls need them; propose the smallest safe fix.",
        output: "Please inspect rust/crates/runtime/src/compact.rs and find why tool-result context survives compaction. Keep error output and file paths because later tool calls depend on them. Propose the smallest safe fix.",
        required_answer_terms: &[
            "rust/crates/runtime/src/compact.rs",
            "tool-result",
            "error output",
            "file paths",
            "smallest safe fix",
        ],
    },
];

fn main() {
    let args = std::env::args().skip(1).collect::<Vec<_>>();
    let mode = args
        .windows(2)
        .find(|args| args[0] == "--mode")
        .map_or_else(|| "caveman".to_string(), |args| args[1].clone());
    let caveman = match mode.as_str() {
        "baseline" => false,
        "caveman" => true,
        other => {
            eprintln!("unknown mode {other:?}; use baseline or caveman");
            std::process::exit(2);
        }
    };

    println!("mode={mode} repeats=3 token_estimator=chars/4");
    let mut totals = [0_usize; 6];

    for sample in SAMPLES {
        println!("sample={}", sample.name);
        for repeat in 1..=3 {
            let input = render(sample.input, caveman);
            let middle = render(sample.middle, caveman);
            let output = render(sample.output, caveman);
            let input_stats = measure_caveman_pair(sample.input, &input);
            let middle_stats = measure_caveman_pair(sample.middle, &middle);
            let output_stats = measure_caveman_pair(sample.output, &output);
            let answer_fidelity = required_answer_fidelity(&output, sample.required_answer_terms);

            assert!(
                answer_fidelity >= 100.0,
                "{} answer lost required term: {output}",
                sample.name
            );
            if caveman {
                assert!(
                    input.chars().count() < sample.input.chars().count(),
                    "{} input chars did not shrink",
                    sample.name
                );
                assert!(
                    middle.chars().count() < sample.middle.chars().count(),
                    "{} middle chars did not shrink",
                    sample.name
                );
                assert!(
                    output.chars().count() < sample.output.chars().count(),
                    "{} output chars did not shrink",
                    sample.name
                );
            }

            totals[0] += input_stats.0;
            totals[1] += input_stats.1;
            totals[2] += middle_stats.0;
            totals[3] += middle_stats.1;
            totals[4] += output_stats.0;
            totals[5] += output_stats.1;

            println!(
                "  run={repeat} input={}→{} tokens middle={}→{} tokens output={}→{} tokens answer_fidelity={answer_fidelity:.1}%",
                input_stats.0,
                input_stats.1,
                middle_stats.0,
                middle_stats.1,
                output_stats.0,
                output_stats.1
            );
            if caveman {
                println!("    output={output}");
            }
        }
    }

    let before = totals[0] + totals[2] + totals[4];
    let after = totals[1] + totals[3] + totals[5];
    let saved = before.saturating_sub(after);
    let savings = if before == 0 {
        0.0
    } else {
        100.0 * saved as f64 / before as f64
    };
    println!(
        "aggregate input={}→{} middle={}→{} output={}→{} total={}→{} saved={} ({savings:.1}%)",
        totals[0], totals[1], totals[2], totals[3], totals[4], totals[5], before, after, saved
    );
    if caveman {
        assert!(after < before, "Caveman run saved no estimated tokens");
    }
}

fn render(text: &str, caveman: bool) -> String {
    if caveman {
        compress_caveman(text)
    } else {
        text.to_string()
    }
}

fn measure_caveman_pair(original: &str, rendered: &str) -> (usize, usize) {
    (estimate_tokens(original), estimate_tokens(rendered))
}

fn required_answer_fidelity(output: &str, required_terms: &[&str]) -> f64 {
    if required_terms.is_empty() {
        return 100.0;
    }
    let output = output.to_ascii_lowercase();
    100.0
        * required_terms
            .iter()
            .filter(|term| output.contains(&term.to_ascii_lowercase()))
            .count() as f64
        / required_terms.len() as f64
}
