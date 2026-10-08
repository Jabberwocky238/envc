use clap::Args;
use envc::{Ctx, Result, Stack as Restore, StateFile};

#[derive(Args, Debug)]
pub struct Stack;

impl Stack {
    pub fn run(&self, ctx: &Ctx) -> Result<()> {
        let env = &ctx.env;
        let path = Restore::path(env)?;
        let c = ctx.palette();
        match Restore::load(env)? {
            Some(stack) => {
                println!(
                    "{} {} ({} frames, {} vars)",
                    c.bold("envc:"),
                    c.bold(stack.chain()),
                    stack.frames.len(),
                    stack.var_count()
                );
                println!("{}", env.tilde(&path));
                println!();
                print!("{}", stack.render());
            }
            None => println!("nothing in use; {} is absent", env.tilde(&path)),
        }
        Ok(())
    }
}
