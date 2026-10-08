use anyhow::anyhow;
use clap::Args;
use envc::{Ctx, Frame, Result, Source, Stack, StateFile};

#[derive(Args, Debug)]
pub struct Unuse {
    #[arg(help = "Profile to take off the stack")]
    name: String,

    #[arg(short, long, help = "Go ahead without asking when a profile on the stack has changed")]
    yes: bool,
}

impl Unuse {
    pub fn run(&self, ctx: &Ctx) -> Result<()> {
        let name = self.name.as_str();
        let env = &ctx.env;
        let mut stack = Stack::load(env)?.unwrap_or_default();
        let Some(i) = stack.position(name) else {
            return Err(anyhow!("'{name}' is not in use ({})", stack.chain()));
        };

        ctx.confirm_drift(&stack.frames[i + 1..], self.yes)?;

        let removed = stack.frames.split_off(i);
        let mut vars = env.vars.clone();
        let mut lines = Vec::new();
        for frame in removed.iter().rev() {
            lines.extend(frame.restore_lines(ctx.shell));
            frame.unwind(&mut vars);
        }

        for old in &removed[1..] {
            let file = env.profile_env_file(&old.profile)?;
            let frame = match file.is_file() {
                true => {
                    let source = Source::load(env, &file, &vars)?;
                    source.archive(env, &old.profile)?;
                    Frame::capture(&old.profile, &vars, &source.assignments, Some(source.digest))
                }
                false => Frame::capture(&old.profile, &vars, &old.assignments(), old.digest.clone()),
            };
            frame.apply(&mut vars);
            lines.extend(frame.apply_lines(ctx.shell));
            stack.frames.push(frame);
        }
        lines.push(stack.active_line(ctx.shell));

        ctx.deliver("unuse", &lines)?;
        stack.store(env)?;
        for frame in removed.iter().rev() {
            frame.record(env, "pop")?;
        }
        for frame in &stack.frames[i..] {
            frame.record(env, "repush")?;
        }

        ctx.notify(format!(
            "popped '{name}' ({} vars restored): {}",
            removed[0].entries.len(),
            stack.chain()
        ));
        ctx.hint(&format!("envc unuse {name}"));
        Ok(())
    }
}
