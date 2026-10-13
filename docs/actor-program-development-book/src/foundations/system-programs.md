# System programs and the block

## The block

A block is a list of transactions. The sequencer adds two of its own at the end: the fee distribution, then the clock tick. The clock transaction must be exactly the one expected for the block's id and timestamp, or the block is invalid (`lez/chain_state/src/apply.rs`).

User transactions cannot modify the clock accounts or the fee accounts. A transaction whose result changes one of them is rejected.

## Native token

The native token is the simplest complete actor program, and worth reading in full (`lee/state_machine/core/src/native_token.rs`).

```rust
pub enum Message {
    Transfer { to: AccountId, amount: Balance },
    Credit(Balance),
    ReadState,
    StateReply(StateReply),
}
```

- `Transfer` needs `is_authorized`. It debits the receiver's own balance and **casts** `Credit` to the destination.
- `Credit` is accepted only from the native token program itself.
- `ReadState` replies to the sender with the balance.

There is one actor per account: `Actor::native_balance(account_id)`. A program that holds funds uses `custody_transfer` with a PDA seed (see [Authorization](authorization.md)).

## Clock

The clock program keeps three actors at fixed accounts (`lez/programs/clock`):

| Account | Updated |
|---|---|
| `CLOCK_01` | every block |
| `CLOCK_10` | every 10th block |
| `CLOCK_50` | every 50th block |

Each stores `{ block_id, timestamp }`. The sequencer sends `Tick` to `CLOCK_01`. On the right blocks, `CLOCK_01` sends `Record` to the slower two. This is the actor rule at work: the every-block actor cannot write the others' state, so it tells them.

A program uses the clock by assertion:

```rust
Response::keep_state().call(
    Actor::new(CLOCK_01_PROGRAM_ACCOUNT_ID, clock_core::clock_account_id()),
    &clock_core::Message::AssertTimestamp { at_least: deadline, at_most: Timestamp::MAX },
)
```

If the clock's timestamp is outside the range, the clock's handler panics and the whole transaction fails. The program never learns the time. It states a condition, and the transaction exists only if the condition holds.

The clock runs last in a block, so transactions in block N see the time of block N-1.

**Clock or validity window?** A [validity window](messages.md) on the response expresses "only valid between these blocks or times" without sending anything. It is checked against the block being built. Use a window for simple deadlines and the clock when you want a named granularity.

**What is missing.** A program cannot read the clock's value, only assert about it. A program that wants to store "now" must be told the time in its message and assert that it is right. A `Stamp` message that replies with the time is sketched in the [idea catalog](../catalog/ideas.md).

## Fee

The fee program holds the fee market's state and distributes each block's fees to the producer. It runs only as the sequencer's system transaction. It is a good example of the reply pattern: it asks a native balance actor for its state and continues when the reply arrives (`lez/programs/fee/src/main.rs`).

## Bridge

The bridge mints deposits made on L1 (`lez/programs/bridge/src/main.rs`). The sequencer injects one transaction per finalized deposit.

- The root goes to a **receipt** actor: a PDA derived from the deposit's id.
- The handler requires the receipt to be empty, writes a marker, and pays the recipient from the bridge's custody PDA.
- A second delivery of the same deposit finds the marker and fails.

The receipt actor is the standard way to make something happen exactly once.

Because of [admission](transactions.md), a deposit to an account that does not exist yet must name the recipient by public key, not by bare id.

## Sequencer stake

The stake program holds the committee's stakes and the channel configuration. Staking, unstaking with an exit delay, and slashing are messages to it. Program authors rarely interact with it.

## Cross-zone messaging

The zone ships inbox and outbox programs for messages between zones (`lez/programs/cross_zone_inbox`, `cross_zone_outbox`). They are not covered in this book.

## What is not there

- No randomness beacon.
- No oracle program.
- No scheduler. Nothing runs unless a transaction delivers a message. Designs that need something to happen "at time T" rely on someone sending a transaction after T.
