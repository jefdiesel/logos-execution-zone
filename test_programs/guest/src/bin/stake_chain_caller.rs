use lee_core::{
    account::{AccountId, ProgramShardSelector},
    program::{ChainedCall, InstructionData, Plan, ProgramCall, read_program_call},
};

/// A program whose only job is to originate a chained call: it forwards a
/// caller-supplied instruction to a caller-supplied program account, passing
/// its own input accounts straight through, shard selections included. Used to
/// reach the `caller_account_id` guards in programs that reject being invoked
/// as a chained call, e.g. `sequencer_stake`'s `Stake` (top-level only).
type Instruction = (AccountId, InstructionData);

fn main() {
    let ProgramCall::Plan(input, (target_program_account_id, forwarded_instruction_data)) =
        read_program_call::<Instruction>()
    else {
        panic!("stake_chain_caller emits no effect to apply")
    };

    // Forward the same accounts, in the same order and on the same shards, into
    // the chained call. The callee sees this program as its `caller_account_id`.
    // No effect of its own: this program exists only to be the caller of
    // `target_program_account_id`, never to mutate state itself.
    let mut plan = Plan::new(&input);
    plan.call(ChainedCall {
        program_account_id: target_program_account_id,
        shard_selectors: input
            .accounts
            .iter()
            .map(ProgramShardSelector::from)
            .collect(),
        instruction_data: forwarded_instruction_data,
        pda_seeds: Vec::new(),
    });
    plan.write();
}
