use clap::Args;
use envc::{Ctx, Loader, Result, Startup};

#[derive(Args, Debug)]
pub struct Init;

impl Init {
    pub fn run(&self, ctx: &Ctx) -> Result<()> {
        let platform = ctx.platform();
        let already_on = platform.is_on()?;

        for line in platform.prepare()? {
            println!("{line}");
        }
        match Startup::current(&ctx.env)? {
            Some(profile) => println!("profile: {profile}"),
            None => println!("profile: none -- `envc enable <name>` picks one"),
        }
        platform.check_environment();

        let advice = platform.advice(already_on)?;
        if !advice.is_empty() {
            println!();
            advice.iter().for_each(|line| println!("{line}"));
        }
        if !ctx.quiet {
            println!();
            println!("next: envc create work, then envc enable work");
        }
        Ok(())
    }
}
