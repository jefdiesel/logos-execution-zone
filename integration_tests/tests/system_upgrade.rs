#![expect(
    clippy::tests_outside_test_module,
    reason = "We don't care about these in tests"
)]

use std::{collections::BTreeMap, time::Duration};

use anyhow::{Context as _, Result};
use common::transaction::LeeTransaction;
use integration_tests::{
    TestContext, get_account_view, new_account, wait_for_indexer_to_catch_up, wait_until,
};
use lee::{AccountId, Actor};
use lee_core::program::{PROGRAM_LOADER_ACCOUNT_ID, ProgramHeader, ProgramId};
use sequencer_service_rpc::RpcClient as _;
use test_fixtures::{
    MultiZoneTestContextBuilder, ZoneTestContextBuilder,
    config::{MultiNodeTestContextConfig, SequencerPartialConfig},
};
use tokio::test;
use wallet::program_facades::program_loader::ProgramLoader;

/// Blocks between scheduling and the upgrade's height.
const UPGRADE_DELAY: u64 = 5;

fn fast_blocks() -> SequencerPartialConfig {
    SequencerPartialConfig {
        block_create_timeout: Duration::from_secs(2),
        ..SequencerPartialConfig::default()
    }
}

async fn image_id(ctx: &TestContext, program: AccountId) -> Result<ProgramId> {
    let header = get_account_view(ctx, Actor::new(program, PROGRAM_LOADER_ACCOUNT_ID)).await?;
    let header =
        ProgramHeader::from_loader_actor_state(header.data.actor_state(PROGRAM_LOADER_ACCOUNT_ID))
            .context("no program header")?;
    Ok(header.image_id)
}

/// The new code is uploaded and the producer schedules `bridge`'s upgrade, and from the upgrade's
/// height `bridge` runs it while blocks keep coming.
#[test]
async fn a_scheduled_system_upgrade_goes_live_at_its_height() -> Result<()> {
    let new_code = test_programs::scripted();
    let user_elf = new_code.user_elf()?;

    let bridge = programs::bridge_account_id();
    let mut ctx = MultiZoneTestContextBuilder::default()
        .with_zone(
            ZoneTestContextBuilder::new(MultiNodeTestContextConfig::default())
                .with_sequencer_partial_config(fast_blocks()),
        )
        .build()
        .await?;
    let payer = ctx.existing_public_accounts()[0];

    assert_eq!(image_id(&ctx, bridge).await?, programs::bridge().id());

    let mut segment_ids = Vec::new();
    for _ in 0..program_loader_core::segment_count(&user_elf) {
        segment_ids.push(new_account(&mut ctx, false, None).await?);
    }
    let segments = program_loader_core::build_segments(&user_elf, &segment_ids)?;

    // Segments link to the next one, so the chain is written tail first. Only segments: the
    // header stays `bridge`'s own.
    for (segment_id, segment) in segment_ids.iter().zip(segments).rev() {
        let tx_hash = ProgramLoader(ctx.wallet())
            .write_segment(
                *segment_id,
                segment.bytecode,
                segment.next_segment,
                Some(payer),
            )
            .await
            .map_err(|err| anyhow::anyhow!("{err}"))?;
        ctx.wallet().poll_transaction(tx_hash).await?;
    }

    // The producer includes the `Schedule`. Until approval tooling lands, the test hands it to the
    // node directly.
    let now = ctx.sequencer_client().get_last_block_id().await?;
    let from_height = now.saturating_add(UPGRADE_DELAY);
    let registry = system_upgrader_core::registry_actor();
    let schedule = LeeTransaction::Public(lee::PublicTransaction::new(
        lee::public_transaction::Message::try_new(
            registry,
            [registry],
            BTreeMap::new(),
            system_upgrader_core::Message::Schedule {
                name: programs::BRIDGE_NAME,
                first_segment: segment_ids[0],
                from_height,
            },
        )?,
        lee::public_transaction::WitnessSet::from_raw_parts(vec![]),
    ));
    let schedule_hash = schedule.hash();
    ctx.default_sequencer_component()
        .sequencer_handle
        .submit_system_upgrader_tx(schedule)
        .await?;
    wait_until("the Schedule to be included", || async {
        Ok(ctx
            .sequencer_client()
            .get_transaction(schedule_hash)
            .await?
            .is_some())
    })
    .await?;
    // Read before the height, so a chain still short of it after the read was short during it.
    let image_before = image_id(&ctx, bridge).await?;
    if ctx.sequencer_client().get_last_block_id().await? < from_height {
        assert_eq!(
            image_before,
            programs::bridge().id(),
            "nothing changes before the upgrade's height"
        );
    }

    wait_until("bridge to run the new code", || async {
        Ok(image_id(&ctx, bridge).await? == new_code.id())
    })
    .await?;

    // `bridge` now runs the scripted guest: it writes what it is told into its own actor state.
    let target_id = new_account(&mut ctx, false, None).await?;
    let signers = [target_id, payer];
    let nonces = signers
        .into_iter()
        .zip(ctx.wallet_mut().get_accounts_nonces(&signers).await?)
        .collect();
    let written = vec![1_u8, 2, 3];
    let target = Actor::new(target_id, bridge);
    let message = lee::public_transaction::Message::try_new_with_fees(
        target,
        [target],
        nonces,
        test_guest_core::Script::write(written.clone()),
        common::test_utils::test_fee_declaration(payer),
    )?;
    let keys = signers.map(|id| {
        ctx.wallet()
            .get_account_public_signing_key(id)
            .expect("the wallet holds both keys")
    });
    let witness_set = lee::public_transaction::WitnessSet::for_message(&message, &keys);
    let tx_hash = ctx
        .sequencer_client()
        .send_transaction(LeeTransaction::Public(lee::PublicTransaction::new(
            message,
            witness_set,
        )))
        .await?;
    ctx.wallet().poll_transaction(tx_hash).await?;
    let view = get_account_view(&ctx, target).await?;
    assert_eq!(view.data.actor_state(bridge).as_ref(), written.as_slice());

    let upgraded_at = ctx.sequencer_client().get_last_block_id().await?;
    wait_until("blocks to keep coming after the upgrade", || async {
        Ok(ctx.sequencer_client().get_last_block_id().await? > upgraded_at.saturating_add(2))
    })
    .await?;

    // The indexer validates every block, the upgrade's system_upgrader transactions included.
    wait_for_indexer_to_catch_up(&ctx).await?;

    Ok(())
}
