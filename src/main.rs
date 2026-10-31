use clap::Parser;
use serde::{Deserialize, Serialize};
use std::fs::File;
use std::io::Write;
use std::process::Command;

#[derive(Parser, Debug)]
#[command(author, version, about = "AI Git Commit Agent")]
struct Args {
    /// Automatically commit the changes using the generated summary
    #[arg(short, long)]
    commit: bool,

    /// Output only the raw summary (useful for piping)
    #[arg(short, long)]
    raw: bool,
}

#[derive(Serialize)]
struct ChatRequest {
    model: String,
    messages: Vec<Message>,
    temperature: f32,
}

#[derive(Serialize, Deserialize)]
struct Message {
    role: String,
    content: String,
}

#[derive(Deserialize)]
struct ChatResponse {
    choices: Vec<Choice>,
}

#[derive(Deserialize)]
struct Choice {
    message: Message,
}

async fn get_git_diff() -> Result<String, Box<dyn std::error::Error>> {
    // 1. Try staged changes first
    let output = Command::new("git")
        .args(["diff", "--cached"])
        .output()?;

    let mut diff = String::from_utf8_lossy(&output.stdout).to_string();

    // 2. If no staged changes, try unstaged
    if diff.trim().is_empty() {
        let output_unstaged = Command::new("git")
            .args(["diff"])
            .output()?;
        diff = String::from_utf8_lossy(&output_unstaged.stdout).to_string();
    }

    Ok(diff)
}

async fn generate_summary(diff: &str) -> Result<String, Box<dyn std::error::Error>> {
    let client = reqwest::Client::new();
    let url = "http://localhost:1234/v1/chat/completions";

    let request_body = ChatRequest {
        model: "google/gemma-4-31-b".to_string(),
        temperature: 0.7,
        messages: vec![
            Message {
                role: "system".to_string(),
                content: "You are an expert software engineer. Write a detailed, professional git commit summary based on the diff. Focus on the 'why' and 'what'. Max 5 paragraphs.".to_string(),
            },
            Message {
                role: "user".to_string(),
                content: format!("Summarize this diff:\n\n{}", diff),
            },
        ],
    };

    let response = client.post(url)
        .json(&request_body)
        .send()
        .await?
        .json::<ChatResponse>()
        .await?;

    Ok(response.choices[0].message.content.clone())
}

fn execute_commit(summary: &str) -> Result<(), Box<dyn std::error::Error>> {
    let temp_file_path = ".git_commit_msg.tmp";
    let mut file = File::create(temp_file_path)?;
    file.write_all(summary.as_bytes())?;

    let status = Command::new("git")
        .args(["commit", "-F", temp_file_path])
        .status()?;

    // Clean up the temporary file
    std::fs::remove_file(temp_file_path)?;

    if status.success() {
        Ok(())
    } else {
        Err("Git commit command failed".into())
    }
}

#[tokio::main]
async fn main() -> Result<(), Box<dyn std::error::Error>> {
    let args = Args::parse();

    println!("🔍 Analyzing git changes...");
    let diff = get_git_diff().await?;

    if diff.trim().is_empty() {
        println!("✨ No changes detected to summarize.");
        return Ok(());
    }

    let summary = generate_summary(&diff).await?;

    if args.commit {
        println!("📝 Committing changes...");
        execute_commit(&summary)?;
        println!("✅ Successfully committed!");
    } else if args.raw {
        println!("{}", summary);
    } else {
        println!("\n{}\nPROPOSED COMMIT SUMMARY\n{}", "=".repeat(30), summary);
        println!("\n{} (Use --commit to apply this)", "=".repeat(30));
    }

    Ok(())
}

