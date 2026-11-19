use std::path::PathBuf;
use std::process::ExitCode;

use clap::Parser;
use mpr2mpt::convert;

#[derive(Parser)]
#[command(
    version,
    about = "Convert a BioLogic EC-Lab MPR measurement table to MPT"
)]
struct Args {
    /// Input BioLogic MPR file
    input: PathBuf,
    /// Output path [default: next to the input, with the .mpt extension]
    #[arg(short, long, value_name = "PATH")]
    output: Option<PathBuf>,
}

fn main() -> ExitCode {
    let args = Args::parse();
    match convert(&args.input, args.output.as_deref()) {
        Ok(report) => {
            println!(
                "Wrote {} ({} rows, {} columns)",
                report.output.display(),
                report.rows,
                report.columns
            );
            ExitCode::SUCCESS
        }
        Err(error) => {
            eprintln!("error: {error}");
            ExitCode::FAILURE
        }
    }
}
