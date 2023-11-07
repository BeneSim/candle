#[cfg(feature = "mkl")]
extern crate intel_mkl_src;

#[cfg(feature = "accelerate")]
extern crate accelerate_src;

use clap::{Parser, ValueEnum};
use std::io::Write;
use tokenizers::Tokenizer;

use candle::quantized::{ggml_file, gguf_file};
use candle::Tensor;
use candle_transformers::generation::LogitsProcessor;

use candle_transformers::models::quantized_llama as model;
use model::ModelWeights;

const DEFAULT_PROMPT: &str = "My favorite theorem is ";

#[derive(Debug)]
enum Prompt {
    Interactive,
    Chat,
    One(String),
}

#[derive(Clone, Debug, Copy, ValueEnum)]
enum Which {
    #[value(name = "7b")]
    L7b,
    #[value(name = "13b")]
    L13b,
    #[value(name = "70b")]
    L70b,
    #[value(name = "7b-chat")]
    L7bChat,
    #[value(name = "13b-chat")]
    L13bChat,
    #[value(name = "70b-chat")]
    L70bChat,
    #[value(name = "7b-code")]
    L7bCode,
    #[value(name = "13b-code")]
    L13bCode,
    #[value(name = "32b-code")]
    L34bCode,
    #[value(name = "7b-mistral")]
    Mistral7b,
    #[value(name = "7b-mistral-instruct")]
    Mistral7bInstruct,
    #[value(name = "7b-zephyr")]
    Zephyr7b,
}

impl Which {
    fn is_mistral(&self) -> bool {
        match self {
            Self::L7b
            | Self::L13b
            | Self::L70b
            | Self::L7bChat
            | Self::L13bChat
            | Self::L70bChat
            | Self::L7bCode
            | Self::L13bCode
            | Self::L34bCode => false,
            Self::Mistral7b | Self::Mistral7bInstruct | Self::Zephyr7b => true,
        }
    }
}

#[derive(Parser, Debug)]
#[command(author, version, about, long_about = None)]
struct Args {
    /// GGML file to load, typically a .bin file generated by the quantize command from llama.cpp
    #[arg(long)]
    model: Option<String>,

    /// The initial prompt, use 'interactive' for entering multiple prompts in an interactive way
    /// and 'chat' for an interactive model where history of previous prompts and generated tokens
    /// is preserved.
    #[arg(long)]
    prompt: Option<String>,

    /// The length of the sample to generate (in tokens).
    #[arg(short = 'n', long, default_value_t = 100)]
    sample_len: usize,

    /// The tokenizer config in json format.
    #[arg(long)]
    tokenizer: Option<String>,

    /// The temperature used to generate samples, use 0 for greedy sampling.
    #[arg(long, default_value_t = 0.8)]
    temperature: f64,

    /// Nucleus sampling probability cutoff.
    #[arg(long)]
    top_p: Option<f64>,

    /// The seed to use when generating random samples.
    #[arg(long, default_value_t = 299792458)]
    seed: u64,

    /// Enable tracing (generates a trace-timestamp.json file).
    #[arg(long)]
    tracing: bool,

    /// Display the token for the specified prompt.
    #[arg(long)]
    verbose_prompt: bool,

    /// Penalty to be applied for repeating tokens, 1. means no penalty.
    #[arg(long, default_value_t = 1.1)]
    repeat_penalty: f32,

    /// The context size to consider for the repeat penalty.
    #[arg(long, default_value_t = 64)]
    repeat_last_n: usize,

    /// The model size to use.
    #[arg(long, default_value = "7b")]
    which: Which,

    /// Group-Query Attention, use 8 for the 70B version of LLaMAv2.
    #[arg(long)]
    gqa: Option<usize>,
}

impl Args {
    fn tokenizer(&self) -> anyhow::Result<Tokenizer> {
        let tokenizer_path = match &self.tokenizer {
            Some(config) => std::path::PathBuf::from(config),
            None => {
                let api = hf_hub::api::sync::Api::new()?;
                let repo = if self.which.is_mistral() {
                    "mistralai/Mistral-7B-v0.1"
                } else {
                    "hf-internal-testing/llama-tokenizer"
                };
                let api = api.model(repo.to_string());
                api.get("tokenizer.json")?
            }
        };
        Tokenizer::from_file(tokenizer_path).map_err(anyhow::Error::msg)
    }

    fn model(&self) -> anyhow::Result<std::path::PathBuf> {
        let model_path = match &self.model {
            Some(config) => std::path::PathBuf::from(config),
            None => {
                let (repo, filename) = match self.which {
                    Which::L7b => ("TheBloke/Llama-2-7B-GGML", "llama-2-7b.ggmlv3.q4_0.bin"),
                    Which::L13b => ("TheBloke/Llama-2-13B-GGML", "llama-2-13b.ggmlv3.q4_0.bin"),
                    Which::L70b => ("TheBloke/Llama-2-70B-GGML", "llama-2-70b.ggmlv3.q4_0.bin"),
                    Which::L7bChat => (
                        "TheBloke/Llama-2-7B-Chat-GGML",
                        "llama-2-7b-chat.ggmlv3.q4_0.bin",
                    ),
                    Which::L13bChat => (
                        "TheBloke/Llama-2-13B-Chat-GGML",
                        "llama-2-13b-chat.ggmlv3.q4_0.bin",
                    ),
                    Which::L70bChat => (
                        "TheBloke/Llama-2-70B-Chat-GGML",
                        "llama-2-70b-chat.ggmlv3.q4_0.bin",
                    ),
                    Which::L7bCode => ("TheBloke/CodeLlama-7B-GGUF", "codellama-7b.Q8_0.gguf"),
                    Which::L13bCode => ("TheBloke/CodeLlama-13B-GGUF", "codellama-13b.Q8_0.gguf"),
                    Which::L34bCode => ("TheBloke/CodeLlama-34B-GGUF", "codellama-34b.Q8_0.gguf"),
                    Which::Mistral7b => (
                        "TheBloke/Mistral-7B-v0.1-GGUF",
                        "mistral-7b-v0.1.Q4_K_S.gguf",
                    ),
                    Which::Mistral7bInstruct => (
                        "TheBloke/Mistral-7B-Instruct-v0.1-GGUF",
                        "mistral-7b-instruct-v0.1.Q4_K_S.gguf",
                    ),
                    Which::Zephyr7b => (
                        "TheBloke/zephyr-7B-alpha-GGUF",
                        "zephyr-7b-alpha.Q4_K_M.gguf",
                    ),
                };
                let api = hf_hub::api::sync::Api::new()?;
                let api = api.model(repo.to_string());
                api.get(filename)?
            }
        };
        Ok(model_path)
    }
}

fn print_token(next_token: u32, tokenizer: &Tokenizer) {
    // Extracting the last token as a string is complicated, here we just apply some simple
    // heuristics as it seems to work well enough for this example. See the following for more
    // details:
    // https://github.com/huggingface/tokenizers/issues/1141#issuecomment-1562644141
    if let Some(text) = tokenizer.id_to_token(next_token) {
        let text = text.replace('▁', " ");
        let ascii = text
            .strip_prefix("<0x")
            .and_then(|t| t.strip_suffix('>'))
            .and_then(|t| u8::from_str_radix(t, 16).ok());
        match ascii {
            None => print!("{text}"),
            Some(ascii) => {
                if let Some(chr) = char::from_u32(ascii as u32) {
                    if chr.is_ascii() {
                        print!("{chr}")
                    }
                }
            }
        }
        let _ = std::io::stdout().flush();
    }
}

fn format_size(size_in_bytes: usize) -> String {
    if size_in_bytes < 1_000 {
        format!("{}B", size_in_bytes)
    } else if size_in_bytes < 1_000_000 {
        format!("{:.2}KB", size_in_bytes as f64 / 1e3)
    } else if size_in_bytes < 1_000_000_000 {
        format!("{:.2}MB", size_in_bytes as f64 / 1e6)
    } else {
        format!("{:.2}GB", size_in_bytes as f64 / 1e9)
    }
}

fn main() -> anyhow::Result<()> {
    use tracing_chrome::ChromeLayerBuilder;
    use tracing_subscriber::prelude::*;

    let args = Args::parse();
    let device = candle_examples::device(false)?;
    let temperature = if args.temperature == 0. {
        None
    } else {
        Some(args.temperature)
    };
    tracing_subscriber::fmt::init();
    let _guard = if args.tracing {
        let (chrome_layer, guard) = ChromeLayerBuilder::new().build();
        tracing_subscriber::registry().with(chrome_layer).init();
        Some(guard)
    } else {
        None
    };

    println!(
        "avx: {}, neon: {}, simd128: {}, f16c: {}",
        candle::utils::with_avx(),
        candle::utils::with_neon(),
        candle::utils::with_simd128(),
        candle::utils::with_f16c()
    );
    println!(
        "temp: {:.2} repeat-penalty: {:.2} repeat-last-n: {}",
        args.temperature, args.repeat_penalty, args.repeat_last_n
    );

    let model_path = args.model()?;
    let mut file = std::fs::File::open(&model_path)?;
    let start = std::time::Instant::now();

    let mut model = match model_path.extension().and_then(|v| v.to_str()) {
        Some("gguf") => {
            let model = gguf_file::Content::read(&mut file)?;
            let mut total_size_in_bytes = 0;
            for (_, tensor) in model.tensor_infos.iter() {
                let elem_count = tensor.shape.elem_count();
                total_size_in_bytes +=
                    elem_count * tensor.ggml_dtype.type_size() / tensor.ggml_dtype.blck_size();
            }
            println!(
                "loaded {:?} tensors ({}) in {:.2}s",
                model.tensor_infos.len(),
                &format_size(total_size_in_bytes),
                start.elapsed().as_secs_f32(),
            );
            ModelWeights::from_gguf(model, &mut file, &device)?
        }
        Some("ggml" | "bin") | Some(_) | None => {
            let model = ggml_file::Content::read(&mut file, &device)?;
            let mut total_size_in_bytes = 0;
            for (_, tensor) in model.tensors.iter() {
                let elem_count = tensor.shape().elem_count();
                total_size_in_bytes +=
                    elem_count * tensor.dtype().type_size() / tensor.dtype().blck_size();
            }
            println!(
                "loaded {:?} tensors ({}) in {:.2}s",
                model.tensors.len(),
                &format_size(total_size_in_bytes),
                start.elapsed().as_secs_f32(),
            );
            println!("params: {:?}", model.hparams);
            let default_gqa = match args.which {
                Which::L7b
                | Which::L13b
                | Which::L7bChat
                | Which::L13bChat
                | Which::L7bCode
                | Which::L13bCode
                | Which::L34bCode => 1,
                Which::Mistral7b
                | Which::Mistral7bInstruct
                | Which::Zephyr7b
                | Which::L70b
                | Which::L70bChat => 8,
            };
            ModelWeights::from_ggml(model, args.gqa.unwrap_or(default_gqa), &device)?
        }
    };
    println!("model built");

    let tokenizer = args.tokenizer()?;
    let prompt = match args.prompt.as_deref() {
        Some("chat") => Prompt::Chat,
        Some("interactive") => Prompt::Interactive,
        Some(s) => Prompt::One(s.to_string()),
        None => Prompt::One(DEFAULT_PROMPT.to_string()),
    };

    let mut pre_prompt_tokens = vec![];
    loop {
        let prompt_str = match &prompt {
            Prompt::One(prompt) => prompt.clone(),
            Prompt::Interactive | Prompt::Chat => {
                print!("> ");
                std::io::stdout().flush()?;
                let mut prompt = String::new();
                std::io::stdin().read_line(&mut prompt)?;
                if prompt.ends_with('\n') {
                    prompt.pop();
                    if prompt.ends_with('\r') {
                        prompt.pop();
                    }
                }
                if args.which.is_mistral() {
                    format!("[INST] {prompt} [/INST]")
                } else {
                    prompt
                }
            }
        };
        print!("{}", &prompt_str);
        let tokens = tokenizer
            .encode(prompt_str, true)
            .map_err(anyhow::Error::msg)?;
        if args.verbose_prompt {
            for (token, id) in tokens.get_tokens().iter().zip(tokens.get_ids().iter()) {
                let token = token.replace('▁', " ").replace("<0x0A>", "\n");
                println!("{id:7} -> '{token}'");
            }
        }

        let prompt_tokens = [&pre_prompt_tokens, tokens.get_ids()].concat();
        let to_sample = args.sample_len.saturating_sub(1);
        let prompt_tokens = if prompt_tokens.len() + to_sample > model::MAX_SEQ_LEN - 10 {
            let to_remove = prompt_tokens.len() + to_sample + 10 - model::MAX_SEQ_LEN;
            prompt_tokens[prompt_tokens.len().saturating_sub(to_remove)..].to_vec()
        } else {
            prompt_tokens
        };
        let mut all_tokens = vec![];
        let mut logits_processor = LogitsProcessor::new(args.seed, temperature, args.top_p);

        let start_prompt_processing = std::time::Instant::now();
        let mut next_token = {
            let input = Tensor::new(prompt_tokens.as_slice(), &device)?.unsqueeze(0)?;
            let logits = model.forward(&input, 0)?;
            let logits = logits.squeeze(0)?;
            // TODO Remove this once implementation is finished.
            let logits = logits.ones_like()?;
            // logits_processor.sample(&logits)?
            15043
        };
        let prompt_dt = start_prompt_processing.elapsed();
        all_tokens.push(next_token);
        print_token(next_token, &tokenizer);

        let eos_token = *tokenizer.get_vocab(true).get("</s>").unwrap();

        let start_post_prompt = std::time::Instant::now();
        for index in 0..to_sample {
            let input = Tensor::new(&[next_token], &device)?.unsqueeze(0)?;
            let logits = model.forward(&input, prompt_tokens.len() + index)?;
            let logits = logits.squeeze(0)?;
            let logits = if args.repeat_penalty == 1. {
                logits
            } else {
                let start_at = all_tokens.len().saturating_sub(args.repeat_last_n);
                candle_transformers::utils::apply_repeat_penalty(
                    &logits,
                    args.repeat_penalty,
                    &all_tokens[start_at..],
                )?
            };
            // TODO Remove this once implementation is finished.
            // let logits = logits.ones_like()?;
            // next_token = logits_processor.sample(&logits)?;
            let next_token = 15043;
            all_tokens.push(next_token);
            print_token(next_token, &tokenizer);
            if next_token == eos_token {
                break;
            };
        }
        let dt = start_post_prompt.elapsed();
        println!(
            "\n\n{:4} prompt tokens processed: {:.2} token/s",
            prompt_tokens.len(),
            prompt_tokens.len() as f64 / prompt_dt.as_secs_f64(),
        );
        println!(
            "{:4} tokens generated: {:.2} token/s",
            to_sample,
            to_sample as f64 / dt.as_secs_f64(),
        );

        match prompt {
            Prompt::One(_) => break,
            Prompt::Interactive => {}
            Prompt::Chat => {
                pre_prompt_tokens = [prompt_tokens.as_slice(), all_tokens.as_slice()].concat()
            }
        }
    }

    Ok(())
}
