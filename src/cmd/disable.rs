use anyhow::anyhow;
use clap::Args;
use envc::{Ctx, Loader, Result, StateFile, Startup};

#[derive(Args, Debug)]
pub struct Disable {
    #[arg(help = "The startup profile to stop loading")]
    name: String,
}

impl Disable {
    pub fn run(&self, ctx: &Ctx) -> Result<()> {
        let name = self.name.as_str();
        match Startup::remembered(&ctx.env)? {
            Some(current) if current == name => {}
            Some(current) => {
                return Err(anyhow!("'{name}' is not the startup profile; '{current}' is"))
            }
            None => return Err(anyhow!("'{name}' is not the startup profile; none is enabled")),
        }

        let platform = ctx.platform();
        let what = platform.revert()?;
        Startup::remove(&ctx.env)?;

        println!("startup loading disabled: {name}");
        match what {
            Some(what) => println!("  {what}"),
            None => println!("  {}", platform.already_off()),
        }
        if !ctx.quiet {
            println!("  this shell keeps its stack -- `envc unuse <name>` drops a profile.");
        }
        Ok(())
    }
}
