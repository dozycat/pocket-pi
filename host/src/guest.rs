//! Guest hosting — one QuickJS realm evaluating one bundled program.
//!
//! This is the pocket-mod pattern (pocketjs/pocket3d/crates/pocket-mod),
//! specialized for the agent domain: no frame(buttons) contract — the guest
//! turn here is `__pi_dispatch(batchJson)` (see spec/spec.ts), and the host
//! pump in main.rs decides when turns happen. The realm is capability-free:
//! a guest can affect exactly what the mounted `pi` surface expresses.

use anyhow::{anyhow, Result};
use rquickjs::{CatchResultExt, Context, Ctx, Function, Object, Runtime};

pub struct Guest {
    rt: Runtime,
    ctx: Context,
}

impl Guest {
    /// Create an empty realm with `console.*` installed (log/info/debug to
    /// stdout, warn/error to stderr — an agent runtime's console IS its UI)
    /// and unhandled promise rejections reported: an agent loop that dies
    /// silently is the one failure mode this runtime exists to prevent.
    pub fn new() -> Result<Guest> {
        let rt = Runtime::new()?;
        rt.set_host_promise_rejection_tracker(Some(Box::new(
            |ctx: Ctx, _promise, reason: rquickjs::Value, is_handled: bool| {
                if is_handled {
                    return;
                }
                let text = ctx
                    .globals()
                    .get::<_, Function>("String")
                    .ok()
                    .and_then(|f| f.call::<_, String>((reason.clone(),)).ok())
                    .unwrap_or_else(|| "<unprintable reason>".into());
                let stack: Option<String> = reason
                    .as_object()
                    .and_then(|o| o.get::<_, String>("stack").ok());
                match stack {
                    Some(stack) => eprintln!("pocket-pi: unhandled rejection: {text}\n{stack}"),
                    None => eprintln!("pocket-pi: unhandled rejection: {text}"),
                }
            },
        )));
        let ctx = Context::full(&rt)?;
        ctx.with(|ctx| install_console(&ctx))
            .map_err(|e| anyhow!("pocket-pi: installing console: {e}"))?;
        Ok(Guest { rt, ctx })
    }

    pub fn with<F, R>(&self, f: F) -> R
    where
        F: FnOnce(Ctx) -> R,
    {
        self.ctx.with(f)
    }

    /// Mount a surface as `globalThis.<name>` (RUNTIMES.md: a surface is one
    /// named namespace populated with op functions).
    pub fn mount<F>(&self, name: &str, build: F) -> Result<()>
    where
        F: for<'js> FnOnce(&Ctx<'js>, &Object<'js>) -> rquickjs::Result<()>,
    {
        self.ctx
            .with(|ctx| -> rquickjs::Result<()> {
                let ns = Object::new(ctx.clone())?;
                build(&ctx, &ns)?;
                ctx.globals().set(name, ns)?;
                Ok(())
            })
            .map_err(|e| anyhow!("pocket-pi: mounting surface '{name}': {e}"))
    }

    /// Evaluate the product bundle (iife script), then drain jobs — the boot
    /// turn. Exceptions come back with the JS stack.
    pub fn eval(&self, label: &str, source: &str) -> Result<()> {
        self.ctx.with(|ctx| -> Result<()> {
            ctx.eval::<(), _>(source.as_bytes())
                .catch(&ctx)
                .map_err(|e| anyhow!("pocket-pi: eval '{label}' failed: {e}"))?;
            Ok(())
        })?;
        self.drain_jobs();
        Ok(())
    }

    /// One guest turn: deliver an event batch to `__pi_dispatch`, then drain
    /// the job queue (Law 3: one guest turn per host tick).
    pub fn dispatch(&self, batch_json: &str) -> Result<()> {
        self.ctx.with(|ctx| -> Result<()> {
            let dispatch: Option<Function> = ctx.globals().get(crate::spec_generated::DISPATCH).ok();
            if let Some(dispatch) = dispatch {
                dispatch
                    .call::<_, ()>((batch_json,))
                    .catch(&ctx)
                    .map_err(|e| anyhow!("pocket-pi: {} threw: {e}", crate::spec_generated::DISPATCH))?;
            }
            Ok(())
        })?;
        self.drain_jobs();
        Ok(())
    }

    /// Drain the microtask/job queue. Job exceptions are logged, not fatal.
    pub fn drain_jobs(&self) {
        loop {
            match self.rt.execute_pending_job() {
                Ok(true) => continue,
                Ok(false) => break,
                Err(e) => eprintln!("pocket-pi: pending job threw: {e:?}"),
            }
        }
    }
}

fn install_console(ctx: &Ctx) -> rquickjs::Result<()> {
    let console = Object::new(ctx.clone())?;

    fn join(args: rquickjs::function::Rest<rquickjs::Value>) -> String {
        let mut out = String::new();
        for (i, v) in args.iter().enumerate() {
            if i > 0 {
                out.push(' ');
            }
            match stringify(v) {
                Some(s) => out.push_str(&s),
                None => out.push_str("<value>"),
            }
        }
        out
    }

    fn stringify(v: &rquickjs::Value) -> Option<String> {
        if let Some(s) = v.as_string() {
            return s.to_string().ok();
        }
        let ctx = v.ctx();
        let to_str: Function = ctx.globals().get("String").ok()?;
        to_str.call::<_, String>((v.clone(),)).ok()
    }

    let out = |args: rquickjs::function::Rest<rquickjs::Value>| println!("{}", join(args));
    let err = |args: rquickjs::function::Rest<rquickjs::Value>| eprintln!("{}", join(args));

    console.set("log", Function::new(ctx.clone(), out)?)?;
    console.set("info", Function::new(ctx.clone(), out)?)?;
    console.set("debug", Function::new(ctx.clone(), out)?)?;
    console.set("warn", Function::new(ctx.clone(), err)?)?;
    console.set("error", Function::new(ctx.clone(), err)?)?;
    ctx.globals().set("console", console)?;
    Ok(())
}
