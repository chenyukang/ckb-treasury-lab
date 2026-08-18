#![no_std]
#![no_main]

ckb_std::entry!(program_entry);
ckb_std::default_alloc!(16384, 2560000, 64);

use alloc::collections::BTreeMap;
use ckb_gen_types::{packed::RawTransaction, prelude::*};
use ckb_std::{
    ckb_constants::Source,
    high_level::{
        QueryIter, load_cell_capacity, load_cell_data, load_cell_lock_hash, load_cell_type,
        load_cell_type_hash, load_header, load_input_since, load_script, load_witness_args,
    },
    since::{LockValue, Since},
    type_id::check_type_id,
};
use treasury_common::{
    BatchWitness, DAO_STATE_NAMESPACE, EVENT_PRESENT, EVENT_STATE_NAMESPACE, Hash, LeafTransition,
    OutPoint, ProposalConfig, ProposalData, ProposalPhase, ProvenBlock, ProvenTransaction,
    TallyPhase, TallyState, TallyWitness, VOTE_STATE_NAMESPACE, VoteData, VoteRecord,
    cbmt_multi_root, namespaced_state_key, transactions_root, verify_smt_transition,
};

const ZERO: Hash = [0; 32];

#[repr(i8)]
enum Error {
    TypeIdInvalid = 1,
    InvalidCellCount,
    InvalidState,
    InvalidTransition,
    ProposalNotFound,
    ProposalInvalid,
    OperatorMissing,
    BondChanged,
    WitnessInvalid,
    BatchLimit,
    CursorInvalid,
    EventOrderInvalid,
    HeaderInvalid,
    TransactionProofInvalid,
    TransactionInvalid,
    EventNotRelevant,
    SmtProofInvalid,
    ReducerMismatch,
    TallyOverflow,
    ChallengeInvalid,
    ChallengePeriodOpen,
    BondOutputInvalid,
    ConfigNotFound,
    ConfigInvalid,
    ContractIdentityMismatch,
    TallyLockInvalid,
}

pub fn program_entry() -> i8 {
    match run() {
        Ok(()) => 0,
        Err(error) => error as i8,
    }
}

fn run() -> Result<(), Error> {
    check_type_id(0, 32).map_err(|_| Error::TypeIdInvalid)?;
    let inputs = QueryIter::new(load_cell_type_hash, Source::GroupInput).count();
    let outputs = QueryIter::new(load_cell_type_hash, Source::GroupOutput).count();
    if inputs > 1 || outputs > 1 || (inputs == 0 && outputs == 0) {
        return Err(Error::InvalidCellCount);
    }
    match (inputs, outputs) {
        (0, 1) => create(),
        (1, 1) => advance(),
        (1, 0) => consume(),
        _ => Err(Error::InvalidTransition),
    }
}

fn create() -> Result<(), Error> {
    let state = load_state(0, Source::GroupOutput)?;
    let (proposal, config) = load_proposal_dep(state.proposal_id)?;
    ensure_tally_identity(&config)?;
    if proposal.phase != ProposalPhase::Closed
        || state.phase != TallyPhase::Active
        || state.sequence != 0
        || state.next_block != proposal.start_block
        || state.next_tx_index != 0
        || state.votes_root != ZERO
        || state.dao_root != ZERO
        || state.events_root != ZERO
        || state.yes != 0
        || state.no != 0
        || state.processed_events != 0
        || state.candidate_since != 0
        || load_cell_lock_hash(0, Source::GroupOutput).map_err(|_| Error::TallyLockInvalid)?
            != state.operator_lock_hash
    {
        return Err(Error::InvalidState);
    }
    require_operator(state.operator_lock_hash)
}

fn advance() -> Result<(), Error> {
    let input = load_state(0, Source::GroupInput)?;
    let output = load_state(0, Source::GroupOutput)?;
    if input.phase != TallyPhase::Active
        || input.proposal_id != output.proposal_id
        || input.operator_lock_hash != output.operator_lock_hash
        || output.sequence
            != input
                .sequence
                .checked_add(1)
                .ok_or(Error::InvalidTransition)?
    {
        return Err(Error::InvalidTransition);
    }
    if load_cell_capacity(0, Source::GroupInput).map_err(|_| Error::BondChanged)?
        != load_cell_capacity(0, Source::GroupOutput).map_err(|_| Error::BondChanged)?
    {
        return Err(Error::BondChanged);
    }
    require_operator(input.operator_lock_hash)?;
    let (proposal, config) = load_proposal_dep(input.proposal_id)?;
    ensure_tally_identity(&config)?;
    let (witness, witness_len) = load_tally_witness()?;
    if witness_len > proposal.max_batch_witness_bytes as usize {
        return Err(Error::BatchLimit);
    }
    let TallyWitness::Advance(batch) = witness else {
        return Err(Error::WitnessInvalid);
    };
    verify_batch(&input, &output, &proposal, &config, &batch)
}

fn consume() -> Result<(), Error> {
    let state = load_state(0, Source::GroupInput)?;
    if state.phase != TallyPhase::Candidate {
        return Err(Error::InvalidTransition);
    }
    match load_tally_witness()?.0 {
        TallyWitness::ChallengeVote {
            challenger_lock_hash,
            omitted,
            event_proof,
        } => {
            let (proposal, config) = load_proposal_dep(state.proposal_id)?;
            ensure_tally_identity(&config)?;
            challenge_vote(
                &state,
                &proposal,
                &config,
                challenger_lock_hash,
                &omitted,
                &event_proof,
            )
        }
        TallyWitness::ChallengeSpend {
            challenger_lock_hash,
            omitted_spend,
            dao_out_point,
            voter_lock_hash,
            event_proof,
            dao_proof,
        } => {
            let (proposal, config) = load_proposal_dep(state.proposal_id)?;
            ensure_tally_identity(&config)?;
            ensure_in_voting_window(&omitted_spend, &proposal)?;
            challenge_spend(
                &state,
                challenger_lock_hash,
                &omitted_spend,
                dao_out_point,
                voter_lock_hash,
                &event_proof,
                &dao_proof,
            )
        }
        TallyWitness::Finalize => {
            let (proposal, config) = load_proposal_input(state.proposal_id)?;
            ensure_tally_identity(&config)?;
            finalize(&state, &proposal)
        }
        TallyWitness::Advance(_) => Err(Error::WitnessInvalid),
    }
}

fn verify_batch(
    input: &TallyState,
    output: &TallyState,
    proposal: &ProposalData,
    config: &ProposalConfig,
    batch: &BatchWitness,
) -> Result<(), Error> {
    let event_count = batch.event_count().ok_or(Error::BatchLimit)?;
    if event_count > proposal.max_events_per_batch as usize
        || batch.state_transitions.len() > proposal.max_state_keys_per_batch as usize
        || output.sequence > proposal.max_batch_sequence as u32
    {
        return Err(Error::BatchLimit);
    }
    let input_cursor = (input.next_block, input.next_tx_index);
    let output_cursor = (batch.end_block, batch.end_tx_index);
    let final_cursor = (
        proposal
            .end_block
            .checked_add(1)
            .ok_or(Error::CursorInvalid)?,
        0,
    );
    if output_cursor <= input_cursor || output_cursor > final_cursor {
        return Err(Error::CursorInvalid);
    }
    let expected_phase = if output_cursor == final_cursor {
        TallyPhase::Candidate
    } else {
        TallyPhase::Active
    };
    let input_lock_hash =
        load_cell_lock_hash(0, Source::GroupInput).map_err(|_| Error::TallyLockInvalid)?;
    let output_lock_hash =
        load_cell_lock_hash(0, Source::GroupOutput).map_err(|_| Error::TallyLockInvalid)?;
    if (expected_phase == TallyPhase::Active && output_lock_hash != input_lock_hash)
        || (expected_phase == TallyPhase::Candidate
            && output_lock_hash != config.candidate_lock_hash)
    {
        return Err(Error::TallyLockInvalid);
    }
    if output.next_block != batch.end_block
        || output.next_tx_index != batch.end_tx_index
        || output.phase != expected_phase
        || output.processed_events
            != input
                .processed_events
                .checked_add(event_count as u64)
                .ok_or(Error::TallyOverflow)?
    {
        return Err(Error::InvalidTransition);
    }
    if expected_phase == TallyPhase::Active {
        if output.candidate_since != 0 {
            return Err(Error::InvalidTransition);
        }
    } else {
        let anchor = latest_header_number()?;
        if anchor < proposal.end_block || output.candidate_since != anchor {
            return Err(Error::InvalidTransition);
        }
    }
    let input_root = unified_root(input).ok_or(Error::InvalidState)?;
    let output_root = unified_root(output).ok_or(Error::InvalidState)?;

    if event_count == 0 {
        if !batch.blocks.is_empty()
            || !batch.previous_vote_records.is_empty()
            || !batch.state_transitions.is_empty()
            || !batch.state_proof.is_empty()
            || output.votes_root != input.votes_root
            || output.dao_root != input.dao_root
            || output.events_root != input.events_root
            || output.yes != input.yes
            || output.no != input.no
        {
            return Err(Error::InvalidTransition);
        }
        return Ok(());
    }

    if !verify_smt_transition(
        input_root,
        output_root,
        &batch.state_proof,
        &batch.state_transitions,
    ) {
        return Err(Error::SmtProofInvalid);
    }

    let mut state_values = transition_map(&batch.state_transitions)?;
    let mut records = initial_records(&batch.previous_vote_records, &state_values)?;
    let mut yes = input.yes;
    let mut no = input.no;
    let mut previous_cursor = None;
    let mut previous_block = None;

    for block in &batch.blocks {
        if previous_block.is_some_and(|number| number >= block.block_number) {
            return Err(Error::EventOrderInvalid);
        }
        previous_block = Some(block.block_number);
        for (tx_index, raw, tx_hash) in verify_proven_block(block)? {
            let cursor = (block.block_number, tx_index);
            if cursor < input_cursor
                || cursor >= output_cursor
                || block.block_number < proposal.start_block
                || block.block_number > proposal.end_block
                || previous_cursor.is_some_and(|previous| cursor <= previous)
            {
                return Err(Error::EventOrderInvalid);
            }
            previous_cursor = Some(cursor);
            let event_value = state_values
                .get_mut(&state_key(EVENT_STATE_NAMESPACE, tx_hash))
                .ok_or(Error::ReducerMismatch)?;
            if *event_value != ZERO {
                return Err(Error::ReducerMismatch);
            }
            *event_value = EVENT_PRESENT;

            let spent_out_points = raw
                .inputs()
                .into_iter()
                .map(|input| unpack_out_point(&input.previous_output()))
                .collect::<alloc::vec::Vec<_>>();
            let mut relevant = false;
            for out_point in &spent_out_points {
                let key = state_key(DAO_STATE_NAMESPACE, out_point.key());
                if let Some(voter) = state_values
                    .get(&key)
                    .copied()
                    .filter(|value| *value != ZERO)
                {
                    remove_record(voter, &mut state_values, &mut records, &mut yes, &mut no)?;
                    relevant = true;
                }
            }

            for (index, output_cell) in raw.outputs().into_iter().enumerate() {
                let Some(type_script) = output_cell.type_().to_opt() else {
                    continue;
                };
                if !is_vote_script(&type_script, config, input.proposal_id) {
                    continue;
                }
                let output_data = raw
                    .outputs_data()
                    .get(index)
                    .ok_or(Error::TransactionInvalid)?
                    .raw_data();
                let vote = VoteData::decode(&output_data).map_err(|_| Error::TransactionInvalid)?;
                if vote.dao_dep_indices.len() > proposal.max_dao_deps_per_vote as usize {
                    return Err(Error::TransactionInvalid);
                }
                let voter_lock_hash = output_cell
                    .lock()
                    .calc_script_hash()
                    .as_slice()
                    .try_into()
                    .unwrap();
                let mut dao_out_points = alloc::vec::Vec::with_capacity(vote.dao_dep_indices.len());
                for dep_index in vote.dao_dep_indices {
                    let cell_dep = raw
                        .cell_deps()
                        .get(dep_index as usize)
                        .ok_or(Error::TransactionInvalid)?;
                    let out_point = unpack_out_point(&cell_dep.out_point());
                    if spent_out_points.contains(&out_point) {
                        return Err(Error::TransactionInvalid);
                    }
                    dao_out_points.push(out_point);
                }
                let record = VoteRecord {
                    voter_lock_hash,
                    direction: vote.direction,
                    amount: vote.amount,
                    block_number: block.block_number,
                    tx_index,
                    dao_out_points,
                };
                apply_record(record, &mut state_values, &mut records, &mut yes, &mut no)?;
                relevant = true;
            }
            if !relevant {
                return Err(Error::EventNotRelevant);
            }
        }
    }

    if yes != output.yes
        || no != output.no
        || !matches_new_values(&state_values, &batch.state_transitions)
    {
        return Err(Error::ReducerMismatch);
    }
    Ok(())
}

fn verify_proven_block(
    proven: &ProvenBlock,
) -> Result<alloc::vec::Vec<(u32, RawTransaction, Hash)>, Error> {
    if proven.transactions.is_empty() {
        return Err(Error::TransactionProofInvalid);
    }
    let header = load_header(proven.header_dep_index as usize, Source::HeaderDep)
        .map_err(|_| Error::HeaderInvalid)?;
    let header_number: u64 = header.raw().number().unpack();
    if header_number != proven.block_number {
        return Err(Error::HeaderInvalid);
    }
    let mut previous_index = None;
    let mut transactions = alloc::vec::Vec::with_capacity(proven.transactions.len());
    let mut indexed_hashes = alloc::vec::Vec::with_capacity(proven.transactions.len());
    for transaction in &proven.transactions {
        if transaction.tx_index >= proven.tx_count
            || previous_index.is_some_and(|index| index >= transaction.tx_index)
        {
            return Err(Error::TransactionProofInvalid);
        }
        previous_index = Some(transaction.tx_index);
        let raw = RawTransaction::from_slice(&transaction.raw_transaction)
            .map_err(|_| Error::TransactionInvalid)?;
        let tx_hash = raw.calc_tx_hash().as_slice().try_into().unwrap();
        indexed_hashes.push((transaction.tx_index, tx_hash));
        transactions.push((transaction.tx_index, raw, tx_hash));
    }
    let raw_root = cbmt_multi_root(proven.tx_count, &indexed_hashes, &proven.lemmas)
        .ok_or(Error::TransactionProofInvalid)?;
    let expected_transactions_root: Hash = header
        .raw()
        .transactions_root()
        .as_slice()
        .try_into()
        .unwrap();
    if transactions_root(raw_root, proven.witnesses_root) != expected_transactions_root {
        return Err(Error::TransactionProofInvalid);
    }
    Ok(transactions)
}

fn challenge_vote(
    state: &TallyState,
    proposal: &ProposalData,
    config: &ProposalConfig,
    challenger: Hash,
    omitted: &ProvenTransaction,
    proof: &[u8],
) -> Result<(), Error> {
    ensure_in_voting_window(omitted, proposal)?;
    let (raw, tx_hash) = verify_proven_transaction(omitted)?;
    let has_vote = raw.outputs().into_iter().any(|output| {
        output
            .type_()
            .to_opt()
            .is_some_and(|script| is_vote_script(&script, config, state.proposal_id))
    });
    let root = unified_root(state).ok_or(Error::InvalidState)?;
    if !has_vote
        || !verify_smt_transition(
            root,
            root,
            proof,
            &[LeafTransition {
                key: state_key(EVENT_STATE_NAMESPACE, tx_hash),
                old_value: ZERO,
                new_value: ZERO,
            }],
        )
    {
        return Err(Error::ChallengeInvalid);
    }
    pay_bond(challenger)
}

fn challenge_spend(
    state: &TallyState,
    challenger: Hash,
    omitted_spend: &ProvenTransaction,
    dao_out_point: OutPoint,
    voter_lock_hash: Hash,
    event_proof: &[u8],
    dao_proof: &[u8],
) -> Result<(), Error> {
    let (spend_tx, spend_hash) = verify_proven_transaction(omitted_spend)?;
    let spends_claimed_dao = spend_tx
        .inputs()
        .into_iter()
        .map(|input| unpack_out_point(&input.previous_output()))
        .any(|out_point| out_point == dao_out_point);
    let root = unified_root(state).ok_or(Error::InvalidState)?;
    if voter_lock_hash == ZERO
        || !spends_claimed_dao
        || !verify_smt_transition(
            root,
            root,
            event_proof,
            &[LeafTransition {
                key: state_key(EVENT_STATE_NAMESPACE, spend_hash),
                old_value: ZERO,
                new_value: ZERO,
            }],
        )
        || !verify_smt_transition(
            root,
            root,
            dao_proof,
            &[LeafTransition {
                key: state_key(DAO_STATE_NAMESPACE, dao_out_point.key()),
                old_value: voter_lock_hash,
                new_value: voter_lock_hash,
            }],
        )
    {
        return Err(Error::ChallengeInvalid);
    }
    pay_bond(challenger)
}

fn finalize(state: &TallyState, proposal: &ProposalData) -> Result<(), Error> {
    let since = Since::new(
        load_input_since(0, Source::GroupInput).map_err(|_| Error::ChallengePeriodOpen)?,
    );
    if !since.flags_is_valid()
        || !since.is_relative()
        || !matches!(
            since.extract_lock_value(),
            Some(LockValue::BlockNumber(blocks)) if blocks >= proposal.challenge_period
        )
    {
        return Err(Error::ChallengePeriodOpen);
    }
    require_operator(state.operator_lock_hash)?;
    pay_bond(state.operator_lock_hash)
}

fn verify_proven_transaction(proven: &ProvenTransaction) -> Result<(RawTransaction, Hash), Error> {
    let header = load_header(proven.header_dep_index as usize, Source::HeaderDep)
        .map_err(|_| Error::HeaderInvalid)?;
    let header_number: u64 = header.raw().number().unpack();
    if header_number != proven.block_number {
        return Err(Error::HeaderInvalid);
    }
    let raw = RawTransaction::from_slice(&proven.raw_transaction)
        .map_err(|_| Error::TransactionInvalid)?;
    let tx_hash = raw.calc_tx_hash().as_slice().try_into().unwrap();
    let raw_root = merkle_raw_root(tx_hash, proven)?;
    let expected_transactions_root: Hash = header
        .raw()
        .transactions_root()
        .as_slice()
        .try_into()
        .unwrap();
    if transactions_root(raw_root, proven.witnesses_root) != expected_transactions_root {
        return Err(Error::TransactionProofInvalid);
    }
    Ok((raw, tx_hash))
}

fn merkle_raw_root(tx_hash: Hash, proven: &ProvenTransaction) -> Result<Hash, Error> {
    use merkle_cbt::MerkleProof;
    use treasury_common::MergeHash;

    if proven.tx_count == 0 || proven.tx_index >= proven.tx_count {
        return Err(Error::TransactionProofInvalid);
    }
    let tree_index = proven
        .tx_count
        .checked_sub(1)
        .and_then(|base| base.checked_add(proven.tx_index))
        .ok_or(Error::TransactionProofInvalid)?;
    let proof = MerkleProof::<Hash, MergeHash>::new(alloc::vec![tree_index], proven.lemmas.clone());
    proof.root(&[tx_hash]).ok_or(Error::TransactionProofInvalid)
}

fn transition_map(transitions: &[LeafTransition]) -> Result<BTreeMap<Hash, Hash>, Error> {
    let mut values = BTreeMap::new();
    for transition in transitions {
        if values
            .insert(transition.key, transition.old_value)
            .is_some()
        {
            return Err(Error::ReducerMismatch);
        }
    }
    Ok(values)
}

fn initial_records(
    provided: &[VoteRecord],
    state_values: &BTreeMap<Hash, Hash>,
) -> Result<BTreeMap<Hash, VoteRecord>, Error> {
    let mut records = BTreeMap::new();
    for record in provided {
        let voter = record.voter_lock_hash;
        let key = state_key(VOTE_STATE_NAMESPACE, voter);
        let expected = state_values.get(&key).ok_or(Error::ReducerMismatch)?;
        if *expected != record.value_hash().map_err(|_| Error::ReducerMismatch)?
            || records.insert(voter, record.clone()).is_some()
        {
            return Err(Error::ReducerMismatch);
        }
    }
    Ok(records)
}

fn remove_record(
    voter: Hash,
    state_values: &mut BTreeMap<Hash, Hash>,
    records: &mut BTreeMap<Hash, VoteRecord>,
    yes: &mut u128,
    no: &mut u128,
) -> Result<(), Error> {
    let record = records.remove(&voter).ok_or(Error::ReducerMismatch)?;
    let vote_value = state_values
        .get_mut(&state_key(VOTE_STATE_NAMESPACE, voter))
        .ok_or(Error::ReducerMismatch)?;
    if *vote_value != record.value_hash().map_err(|_| Error::ReducerMismatch)? {
        return Err(Error::ReducerMismatch);
    }
    *vote_value = ZERO;
    subtract_tally(record.direction, record.amount, yes, no)?;
    for out_point in &record.dao_out_points {
        let dao_value = state_values
            .get_mut(&state_key(DAO_STATE_NAMESPACE, out_point.key()))
            .ok_or(Error::ReducerMismatch)?;
        if *dao_value != voter {
            return Err(Error::ReducerMismatch);
        }
        *dao_value = ZERO;
    }
    Ok(())
}

fn apply_record(
    record: VoteRecord,
    state_values: &mut BTreeMap<Hash, Hash>,
    records: &mut BTreeMap<Hash, VoteRecord>,
    yes: &mut u128,
    no: &mut u128,
) -> Result<(), Error> {
    let voter = record.voter_lock_hash;
    let vote_key = state_key(VOTE_STATE_NAMESPACE, voter);
    if state_values
        .get(&vote_key)
        .copied()
        .ok_or(Error::ReducerMismatch)?
        != ZERO
    {
        remove_record(voter, state_values, records, yes, no)?;
    }
    for out_point in &record.dao_out_points {
        let value = state_values
            .get_mut(&state_key(DAO_STATE_NAMESPACE, out_point.key()))
            .ok_or(Error::ReducerMismatch)?;
        if *value != ZERO {
            return Err(Error::ReducerMismatch);
        }
        *value = voter;
    }
    add_tally(record.direction, record.amount, yes, no)?;
    let value_hash = record.value_hash().map_err(|_| Error::ReducerMismatch)?;
    *state_values
        .get_mut(&vote_key)
        .ok_or(Error::ReducerMismatch)? = value_hash;
    if records.insert(voter, record).is_some() {
        return Err(Error::ReducerMismatch);
    }
    Ok(())
}

fn add_tally(direction: u8, amount: u64, yes: &mut u128, no: &mut u128) -> Result<(), Error> {
    let target = if direction == 1 { yes } else { no };
    *target = target
        .checked_add(amount as u128)
        .ok_or(Error::TallyOverflow)?;
    Ok(())
}

fn subtract_tally(direction: u8, amount: u64, yes: &mut u128, no: &mut u128) -> Result<(), Error> {
    let target = if direction == 1 { yes } else { no };
    *target = target
        .checked_sub(amount as u128)
        .ok_or(Error::ReducerMismatch)?;
    Ok(())
}

fn matches_new_values(values: &BTreeMap<Hash, Hash>, transitions: &[LeafTransition]) -> bool {
    transitions
        .iter()
        .all(|transition| values.get(&transition.key) == Some(&transition.new_value))
}

fn unified_root(state: &TallyState) -> Option<Hash> {
    if state.votes_root == state.dao_root && state.dao_root == state.events_root {
        Some(state.votes_root)
    } else {
        None
    }
}

fn state_key(namespace: u8, key: Hash) -> Hash {
    namespaced_state_key(namespace, key).expect("fixed state namespace")
}

fn unpack_out_point(out_point: &ckb_gen_types::packed::OutPoint) -> OutPoint {
    OutPoint {
        tx_hash: out_point.tx_hash().as_slice().try_into().unwrap(),
        index: out_point.index().unpack(),
    }
}

fn is_vote_script(
    script: &ckb_gen_types::packed::Script,
    config: &ProposalConfig,
    proposal_id: Hash,
) -> bool {
    script.code_hash().as_slice() == config.vote_code_hash
        && script.hash_type().as_slice()[0] == config.vote_hash_type
        && script.args().raw_data().as_ref() == proposal_id
}

fn ensure_in_voting_window(
    proven: &ProvenTransaction,
    proposal: &ProposalData,
) -> Result<(), Error> {
    if proven.block_number < proposal.start_block || proven.block_number > proposal.end_block {
        Err(Error::ChallengeInvalid)
    } else {
        Ok(())
    }
}

fn require_operator(operator_lock_hash: Hash) -> Result<(), Error> {
    if QueryIter::new(load_cell_lock_hash, Source::Input)
        .any(|lock_hash| lock_hash == operator_lock_hash)
    {
        Ok(())
    } else {
        Err(Error::OperatorMissing)
    }
}

fn pay_bond(recipient_lock_hash: Hash) -> Result<(), Error> {
    let bond = load_cell_capacity(0, Source::GroupInput).map_err(|_| Error::BondOutputInvalid)?;
    let mut found = None;
    for (index, lock_hash) in QueryIter::new(load_cell_lock_hash, Source::Output).enumerate() {
        if lock_hash == recipient_lock_hash && found.replace(index).is_some() {
            return Err(Error::BondOutputInvalid);
        }
    }
    let index = found.ok_or(Error::BondOutputInvalid)?;
    if load_cell_capacity(index, Source::Output).map_err(|_| Error::BondOutputInvalid)? != bond
        || load_cell_type(index, Source::Output)
            .map_err(|_| Error::BondOutputInvalid)?
            .is_some()
        || !load_cell_data(index, Source::Output)
            .map_err(|_| Error::BondOutputInvalid)?
            .is_empty()
    {
        return Err(Error::BondOutputInvalid);
    }
    Ok(())
}

fn load_state(index: usize, source: Source) -> Result<TallyState, Error> {
    let data = load_cell_data(index, source).map_err(|_| Error::InvalidState)?;
    TallyState::decode(&data).map_err(|_| Error::InvalidState)
}

fn load_proposal_dep(proposal_id: Hash) -> Result<(ProposalData, ProposalConfig), Error> {
    load_proposal(proposal_id, Source::CellDep)
}

fn load_proposal_input(proposal_id: Hash) -> Result<(ProposalData, ProposalConfig), Error> {
    load_proposal(proposal_id, Source::Input)
}

fn load_proposal(
    proposal_id: Hash,
    source: Source,
) -> Result<(ProposalData, ProposalConfig), Error> {
    for (index, type_hash) in QueryIter::new(load_cell_type_hash, source).enumerate() {
        if type_hash == Some(proposal_id) {
            let type_script = load_cell_type(index, source)
                .map_err(|_| Error::ProposalInvalid)?
                .ok_or(Error::ProposalInvalid)?;
            let data = load_cell_data(index, source).map_err(|_| Error::ProposalInvalid)?;
            let proposal = ProposalData::decode(&data).map_err(|_| Error::ProposalInvalid)?;
            if proposal.phase != ProposalPhase::Closed {
                return Err(Error::ProposalInvalid);
            }
            let config = load_proposal_config(proposal.proposal_config_type_hash)?;
            if type_script.code_hash().as_slice() != config.proposal_code_hash
                || type_script.hash_type().as_slice()[0] != config.proposal_hash_type
            {
                return Err(Error::ContractIdentityMismatch);
            }
            return Ok((proposal, config));
        }
    }
    Err(Error::ProposalNotFound)
}

fn load_proposal_config(config_type_hash: Hash) -> Result<ProposalConfig, Error> {
    for (index, type_hash) in QueryIter::new(load_cell_type_hash, Source::CellDep).enumerate() {
        if type_hash == Some(config_type_hash) {
            let data = load_cell_data(index, Source::CellDep).map_err(|_| Error::ConfigInvalid)?;
            return ProposalConfig::decode(&data).map_err(|_| Error::ConfigInvalid);
        }
    }
    Err(Error::ConfigNotFound)
}

fn ensure_tally_identity(config: &ProposalConfig) -> Result<(), Error> {
    let script = load_script().map_err(|_| Error::ContractIdentityMismatch)?;
    if script.code_hash().as_slice() == config.tally_code_hash
        && script.hash_type().as_slice()[0] == config.tally_hash_type
    {
        Ok(())
    } else {
        Err(Error::ContractIdentityMismatch)
    }
}

fn load_tally_witness() -> Result<(TallyWitness, usize), Error> {
    let witness = load_witness_args(0, Source::GroupInput).map_err(|_| Error::WitnessInvalid)?;
    let input_type = witness
        .input_type()
        .to_opt()
        .ok_or(Error::WitnessInvalid)?
        .raw_data();
    let len = input_type.len();
    TallyWitness::decode(&input_type)
        .map(|witness| (witness, len))
        .map_err(|_| Error::WitnessInvalid)
}

fn latest_header_number() -> Result<u64, Error> {
    QueryIter::new(load_header, Source::HeaderDep)
        .map(|header| header.raw().number().unpack())
        .max()
        .ok_or(Error::HeaderInvalid)
}
