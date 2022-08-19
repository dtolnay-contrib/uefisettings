// (c) Meta Platforms, Inc. and affiliates. Confidential and proprietary.

use std::fmt::Debug;
use std::fs::File;
use std::io::Read;
use std::io::Write;
use std::path::Path;
use std::path::PathBuf;
use std::process;

use anyhow::anyhow;
use anyhow::Context;
use anyhow::Result;
use clap::Parser;
use clap::Subcommand;
use fbthrift::simplejson_protocol;
use fbthrift::simplejson_protocol::Serializable;
use log::info;
use uefisettingslib::exports::HiiBackend;
use uefisettingslib::exports::IloBackend;
use uefisettingslib::exports::SettingsBackend;

const MAX_ALLOWED_FILESIZE: u64 = 16 * 1024 * 1024;

#[derive(Debug, Parser)]
#[clap(
    name = "uefisettings",
    about = "UEFI Settings Manipulation Tool",
    long_about = None
)]
struct UefiSettingsToolArgs {
    #[clap(subcommand)]
    command: Commands,
}

#[derive(Debug, Subcommand)]
enum Commands {
    /// Commands which work on machines exposing the UEFI HiiDB
    Hii(HiiCommand),
    /// Commands which work on machines having HPE's Ilo BMC
    Ilo(IloCommand),
    // TODO: Auto(AutoCommand) after building a backend identifier function
    // TODO: Get/Set without having to specify Auto
}

#[derive(Debug, Parser)]
struct HiiCommand {
    #[clap(subcommand)]
    command: HiiSubcommands,
}

#[derive(Debug, Subcommand)]
enum HiiSubcommands {
    /// Get the current value of a question
    Get {
        #[clap(value_parser)]
        question: String,
        #[clap(short = 'j', long = "json", action, value_parser)]
        machine_readable: bool,
    },
    /// Set/change the value of a question
    Set {
        #[clap(value_parser)]
        question: String,
        #[clap(value_parser)]
        value: String,
        #[clap(short = 'j', long = "json", action, value_parser)]
        machine_readable: bool,
    },
    /// Show a human readable representation of the Hii Forms
    ShowIFR {
        /// If filename of HiiDB isn't specified then this tool will try to automatically extract it
        #[clap(parse(from_os_str), short, long)]
        filename: Option<PathBuf>,
    },
    /// Dump HiiDB into a file
    ExtractDB {
        #[clap(parse(from_os_str))]
        filename: PathBuf,
    },
    /// List all strings-id, string pairs in HiiDB
    ListStrings {
        /// If filename of HiiDB isn't specified then this tool will try to automatically extract it
        #[clap(parse(from_os_str), short, long)]
        filename: Option<PathBuf>,
        #[clap(short = 'j', long = "json", action, value_parser)]
        machine_readable: bool,
    },
}

#[derive(Debug, Parser)]
struct IloCommand {
    #[clap(subcommand)]
    command: IloSubcommands,
}

#[derive(Debug, Subcommand)]
enum IloSubcommands {
    /// Get the current value of a question
    Get {
        #[clap(value_parser)]
        question: String,
        #[clap(short = 'j', long = "json", action, value_parser)]
        machine_readable: bool,
    },
    /// Set/change the value of a question
    Set {
        #[clap(value_parser)]
        question: String,
        #[clap(value_parser)]
        value: String,
        #[clap(short = 'j', long = "json", action, value_parser)]
        machine_readable: bool,
    },
    /// List bios attributes and their current values
    ShowAttributes {
        #[clap(short = 'j', long = "json", action, value_parser)]
        machine_readable: bool,
    },
    // TODO: add more ilo specific commands - GetPending and ShowPendingAttributes
}

fn main() -> Result<()> {
    env_logger::init();
    let args = UefiSettingsToolArgs::parse();

    if let Err(why) = handle_cmds(args) {
        println!("{{\"error\": \"{:?}\"}}", why);
        process::exit(1);
    }

    info!("Exiting UEFI Settings Manipulation Tool");
    Ok(())
}

fn handle_cmds(args: UefiSettingsToolArgs) -> Result<()> {
    match &args.command {
        Commands::Hii(hii_command) => match &hii_command.command {
            HiiSubcommands::Get {
                question,
                machine_readable,
            } => {
                let res = HiiBackend::get(question, None)?;
                print_with_style(res, *machine_readable);
            }
            HiiSubcommands::Set {
                question,
                value,
                machine_readable,
            } => {
                let res = HiiBackend::set(question, value, None)?;
                print_with_style(res, *machine_readable);
            }
            HiiSubcommands::ShowIFR { filename } => {
                let res = HiiBackend::show_ifr(&get_db_dump_bytes(filename.as_deref())?)?;
                println!("{}", res.readable_representation);
            }
            HiiSubcommands::ExtractDB { filename } => {
                let mut file = File::create(filename)?;
                let res = HiiBackend::extract_db()?;
                file.write_all(&res.db)?;

                println!("{{\"info\": \"HiiDB written to {:?}\"}}", &filename);
            }
            HiiSubcommands::ListStrings {
                filename,
                machine_readable,
            } => {
                let res = HiiBackend::list_strings(&get_db_dump_bytes(filename.as_deref())?)?;
                print_with_style(res, *machine_readable);
            }
        },
        Commands::Ilo(ilo_command) => match &ilo_command.command {
            IloSubcommands::Get {
                question,
                machine_readable,
            } => {
                let res = IloBackend::get(question, None)?;
                print_with_style(res, *machine_readable);
            }
            IloSubcommands::Set {
                question,
                value,
                machine_readable,
            } => {
                let res = IloBackend::set(question, value, None)?;
                print_with_style(res, *machine_readable);
            }
            IloSubcommands::ShowAttributes { machine_readable } => {
                let res = IloBackend::show_attributes()?;
                print_with_style(res, *machine_readable);
            }
        },
    }
    Ok(())
}

fn get_db_dump_bytes(filename: Option<&Path>) -> Result<Vec<u8>> {
    // If dbdump's path is provided use that
    if let Some(dbdump_path) = filename {
        info!("Using database dump from file: {}", dbdump_path.display());

        let mut file = File::open(&dbdump_path)
            .context(format!("opening dbdump from{}", dbdump_path.display()))?;

        // Most Hii DBs are afew hundred kilobytes in size and the largest we've seen so far is close to 3 MB.
        // Since we're reading the entire DB into a Vec<u8> we need to have a check here.
        if file
            .metadata()
            .context("failed to read metadata for open file")?
            .len()
            > MAX_ALLOWED_FILESIZE
        {
            return Err(anyhow!(
                "File size is too big for the file to be a HII database."
            ));
        }

        let mut file_contents = Vec::new();
        file.read_to_end(&mut file_contents)
            .context("Couldn't convert file bytes to Vec<u8>")?;

        Ok(file_contents)
    } else {
        Ok(HiiBackend::extract_db()?.db)
    }
}

// print_with_style either prints as json or with rust's debug pretty-printer
fn print_with_style<T>(result: T, machine_readable: bool)
where
    T: Serializable + Debug,
{
    if machine_readable {
        let buf = simplejson_protocol::serialize(result);
        println!("{}", String::from_utf8_lossy(&buf));
    } else {
        println!("{:#?}", result);
    }
}
