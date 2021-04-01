#![feature(type_ascription)]
#![feature(box_syntax)]
#![feature(core_intrinsics)]

use anyhow::*;
use hashbrown::HashMap;
use structopt::StructOpt;
use xactor::Actor;

use config::{Config, SubCommand};
use crate::utils::CheckError;
use crate::database::{DbMsg, DbReply};
use crate::trace::TraceActor;
use crate::database::DbMsg::Get;
use std::sync::atomic::{AtomicBool, AtomicUsize};
use std::sync::Arc;
use std::cmp::Ordering;
use std::sync::atomic::Ordering::SeqCst;
use std::hint::spin_loop;

mod database;
mod config;
mod socket;
mod status;
mod client;
mod trace;
mod utils;

#[global_allocator]
static GLOBAL: snmalloc_rs::SnMalloc = snmalloc_rs::SnMalloc;

#[async_std::main]
async fn main() -> Result<()> {
    pretty_env_logger::try_init_timed_custom_env("GIRASOL_LOG_LEVEL")?;
    let conf: Config = config::Config::from_args();
    let db = database::init(&conf.home).await?;
    let mut db_actor = database::DataActor::new(db).start().await;
    match conf.subcommand {
        SubCommand::Endpoint { server } => {
            let (mut rd, wt) = socket::create_sockets(&server).await?;
            let mut send_client = client::SendClient::new(wt).start().await;
            let mut keeper = trace::HouseKeeper {
                send_client: send_client.clone(),
                running_trace: HashMap::new(),
            }.start().await;
            let handle = std::cell::UnsafeCell::new(db_actor.clone());
            ctrlc::set_handler(move || unsafe {
                async_std::task::block_on((*handle.get()).call(DbMsg::Kill)).check_error();
                std::process::exit(0);
            })?;
            rd.listen(db_actor.clone(), send_client.clone(), keeper.clone()).await;
            keeper.stop(None)?;
            send_client.stop(None)?;
        }
        SubCommand::List { detail } => {
            config::handle_list(db_actor.clone(), detail).await;
        }
        SubCommand::Add { editor } => {
            config::handle_add(db_actor.clone(), editor).await;
        }
        SubCommand::Check { name } => {
            config::handle_check(db_actor.clone(), name).await;
        }
        SubCommand::Remove { name } => {
            config::handle_remove(db_actor.clone(), name).await;
        }
        SubCommand::Local { name, round, pattern } => {
            let written = Arc::new(AtomicUsize::new(round));
            if let DbReply::GetResult(model) = db_actor.call(Get(name)).await?? {
                let actor = TraceActor {
                    house_keeper: None,
                    send_client: None,
                    model,
                    file: None,
                    child: None,
                    written: written.clone(),
                    pattern
                };
                let mut addr = actor.start().await;
                while written.load(SeqCst) != 0 {
                    spin_loop();
                }
                addr.stop(None)?;
            }
        }
    }
    db_actor.call(DbMsg::Kill).await.check_error();
    Ok(())
}


