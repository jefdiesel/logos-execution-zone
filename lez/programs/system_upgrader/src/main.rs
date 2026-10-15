//! System Upgrader Program.
//!
//! Owns every system program's address as a PDA and upgrades a system program in two steps:
//! `Schedule` records the new segment chain and the block it applies from in the registry; `Apply`
//! checks and clears that record, then has the program loader point the program's header at the
//! new chain, authorized through the program's PDA seed. `Cancel` clears a pending record without
//! upgrading, and `Install` deploys a new system program the same way. Every message is handled at
//! the registry's actor.

use lee_core::program::{LoaderMessage, PdaSeed, ReceiveInput, Response, SyncMessage};
use system_upgrader_core::{
    Message, Registry, ScheduledUpgrade, SystemProgramName, loader_actor, registry_account_id,
};

lee_core::actor_entrypoint!(handle_message);

fn handle_message(input: &ReceiveInput, message: Message) -> Response {
    assert!(
        input.from.is_none(),
        "system_upgrader may only be invoked at the top level"
    );
    assert_eq!(
        input.receiver.account_id,
        registry_account_id(),
        "system_upgrader messages are handled at its registry"
    );
    let mut registry = Registry::from_bytes(&input.pre_state).expect("registry decodes");

    match message {
        Message::Schedule {
            name,
            first_segment,
            from_height,
        } => {
            let pending = pending_mut(&mut registry, &name);
            assert!(
                pending.is_none(),
                "system program already has a scheduled upgrade"
            );
            *pending = Some(ScheduledUpgrade {
                first_segment,
                from_height,
            });
            Response::set_state(registry.to_bytes())
        }
        Message::Apply { name, from_height } => {
            let upgrade = pending_mut(&mut registry, &name)
                .take()
                .expect("system program has no scheduled upgrade");
            assert_eq!(
                upgrade.from_height, from_height,
                "the upgrade does not match the system program's schedule"
            );
            Response::set_state(registry.to_bytes())
                .block_window(from_height..)
                .send(loader_message(
                    &name,
                    &LoaderMessage::UpdateHeader {
                        first_segment: upgrade.first_segment,
                        immutable: false,
                    },
                ))
        }
        Message::Cancel {
            name,
            first_segment,
            from_height,
        } => {
            let pending = pending_mut(&mut registry, &name);
            assert_eq!(
                *pending,
                Some(ScheduledUpgrade {
                    first_segment,
                    from_height,
                }),
                "the upgrade does not match the system program's schedule"
            );
            *pending = None;
            Response::set_state(registry.to_bytes())
        }
        Message::Install {
            name,
            first_segment,
        } => {
            assert!(
                registry.programs.insert(name, None).is_none(),
                "system program is already registered"
            );
            Response::set_state(registry.to_bytes()).send(loader_message(
                &name,
                &LoaderMessage::CreateHeader {
                    first_segment,
                    immutable: false,
                },
            ))
        }
    }
}

/// The registry entry for `name`, which must be registered.
fn pending_mut<'registry>(
    registry: &'registry mut Registry,
    name: &SystemProgramName,
) -> &'registry mut Option<ScheduledUpgrade> {
    registry
        .programs
        .get_mut(name)
        .expect("system program is not registered")
}

/// A program loader message to the system program `name`'s header, authorized by its PDA seed.
fn loader_message(name: &SystemProgramName, message: &LoaderMessage) -> SyncMessage {
    SyncMessage::new(loader_actor(name), message)
        .with_pda_seeds([PdaSeed::for_system_program(name)])
}
