use anyhow::Result;
use clap::Parser;
use recovery::recovery;

#[derive(Parser)]
struct Cli {
    #[arg(short, long)]
    auto: bool,
}

fn main() -> Result<()> {
    let cli = Cli::parse();
    let config = recovery::Config { auto_start: cli.auto, ..Default::default() };
    let mut component = recovery::new(config);
    
    component.initialize()?;
    component.run()?;
    
    println!("SigmaOS-recovery operational");
    Ok(())
}
