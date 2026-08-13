use std::collections::{BTreeMap, BTreeSet};

use ckb_gen_types::{packed::RawTransaction, prelude::*};
use merkle_cbt::CBMT;
use sparse_merkle_tree::{
    H256, SparseMerkleTree, blake2b::Blake2bHasher, default_store::DefaultStore,
};
use treasury_common::{
    BatchWitness, EVENT_PRESENT, Hash, LeafTransition, MergeHash, OutPoint, ProposalData,
    ProvenTransaction, TallyPhase, TallyState, VoteData, VoteRecord,
};

mod rpc;

pub use rpc::{CkbRpcClient, RpcError};

type Smt = SparseMerkleTree<Blake2bHasher, H256, DefaultStore<H256>>;
const ZERO: Hash = [0; 32];

#[derive(Debug)]
pub enum BuilderError {
    InvalidState,
    InvalidEvent,
    BatchLimit,
    CursorInvalid,
    MissingStateKey,
    TallyOverflow,
    Encoding,
    Smt,
}

#[derive(Debug)]
pub enum ScanError<E> {
    Builder(BuilderError),
    Source(E),
}

impl<E> From<BuilderError> for ScanError<E> {
    fn from(error: BuilderError) -> Self {
        Self::Builder(error)
    }
}

pub trait ChainSource {
    type Error;

    fn block_by_number(&self, block_number: u64) -> Result<ChainBlock, Self::Error>;

    fn submit_transaction(
        &self,
        transaction: ckb_gen_types::packed::Transaction,
    ) -> Result<Hash, Self::Error>;
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ChainBlock {
    pub block_number: u64,
    pub block_hash: Hash,
    pub parent_hash: Hash,
    pub raw_transactions: Vec<Vec<u8>>,
    pub witnesses_root: Hash,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ScannedBatch {
    pub events: Vec<ProvenTransaction>,
    pub header_deps: Vec<Hash>,
    pub end_block: u64,
    pub end_tx_index: u32,
    pub candidate_since: u64,
}

pub struct TallyBuilder {
    state: TallyState,
    votes: Smt,
    dao: Smt,
    events: Smt,
    records: BTreeMap<Hash, VoteRecord>,
}

pub struct BlockTransactions {
    pub block_number: u64,
    pub header_dep_index: u16,
    pub raw_transactions: Vec<Vec<u8>>,
    pub witnesses_root: Hash,
}

impl TallyBuilder {
    pub fn new(proposal_id: Hash, operator_lock_hash: Hash, start_block: u64) -> Self {
        Self {
            state: TallyState {
                phase: TallyPhase::Active,
                proposal_id,
                operator_lock_hash,
                sequence: 0,
                next_block: start_block,
                next_tx_index: 0,
                votes_root: ZERO,
                dao_root: ZERO,
                events_root: ZERO,
                yes: 0,
                no: 0,
                processed_events: 0,
                candidate_since: 0,
            },
            votes: Smt::default(),
            dao: Smt::default(),
            events: Smt::default(),
            records: BTreeMap::new(),
        }
    }

    pub fn state(&self) -> &TallyState {
        &self.state
    }

    /// Scans one ordered block and returns exactly the vote and tracked-DAO-spend
    /// transactions that must be included in the next batch.
    pub fn discover_block_events(
        &self,
        proposal: &ProposalData,
        block: &BlockTransactions,
    ) -> Result<Vec<ProvenTransaction>, BuilderError> {
        let mut view = self.clone_builder();
        let mut events = Vec::new();
        for (index, raw_bytes) in block.raw_transactions.iter().enumerate() {
            let raw =
                RawTransaction::from_slice(raw_bytes).map_err(|_| BuilderError::InvalidEvent)?;
            if !view.is_relevant(proposal, &raw)? {
                continue;
            }
            let proven = prove_transaction(
                block.block_number,
                block.header_dep_index,
                &block.raw_transactions,
                block.witnesses_root,
                index as u32,
            )?;
            let mut vote_keys = BTreeSet::new();
            let mut dao_keys = BTreeSet::new();
            let mut event_keys = BTreeSet::new();
            view.apply_event(
                proposal,
                &proven,
                &mut vote_keys,
                &mut dao_keys,
                &mut event_keys,
            )?;
            events.push(proven);
        }
        Ok(events)
    }

    /// Scans consecutive canonical blocks while carrying the temporary reducer
    /// state across block boundaries. The returned header hashes must be added
    /// to the batch transaction in exactly this order.
    pub fn scan_blocks(
        &self,
        proposal: &ProposalData,
        blocks: &[ChainBlock],
    ) -> Result<ScannedBatch, BuilderError> {
        if self.state.phase != TallyPhase::Active || blocks.is_empty() {
            return Err(BuilderError::InvalidState);
        }
        let mut expected_number = self.state.next_block;
        let mut previous_hash = None;
        for block in blocks {
            if block.block_number != expected_number || block.block_number > proposal.end_block {
                return Err(BuilderError::CursorInvalid);
            }
            if previous_hash.is_some_and(|hash| block.parent_hash != hash) {
                return Err(BuilderError::CursorInvalid);
            }
            expected_number = expected_number
                .checked_add(1)
                .ok_or(BuilderError::CursorInvalid)?;
            previous_hash = Some(block.block_hash);
        }

        let mut view = self.clone_builder();
        let mut events = Vec::new();
        let mut header_deps = Vec::new();
        for (block_offset, block) in blocks.iter().enumerate() {
            let start_index = if block_offset == 0 {
                self.state.next_tx_index as usize
            } else {
                0
            };
            if start_index > block.raw_transactions.len() {
                return Err(BuilderError::CursorInvalid);
            }
            for (index, raw_bytes) in block.raw_transactions.iter().enumerate().skip(start_index) {
                let raw = RawTransaction::from_slice(raw_bytes)
                    .map_err(|_| BuilderError::InvalidEvent)?;
                if !view.is_relevant(proposal, &raw)? {
                    continue;
                }
                if events.len() == proposal.max_events_per_batch as usize {
                    return Ok(ScannedBatch {
                        events,
                        header_deps,
                        end_block: block.block_number,
                        end_tx_index: index.try_into().map_err(|_| BuilderError::CursorInvalid)?,
                        candidate_since: 0,
                    });
                }
                let header_dep_index = header_dep_index(&mut header_deps, block.block_hash)?;
                let proven = prove_transaction(
                    block.block_number,
                    header_dep_index,
                    &block.raw_transactions,
                    block.witnesses_root,
                    index.try_into().map_err(|_| BuilderError::InvalidEvent)?,
                )?;
                let mut vote_keys = BTreeSet::new();
                let mut dao_keys = BTreeSet::new();
                let mut event_keys = BTreeSet::new();
                view.apply_event(
                    proposal,
                    &proven,
                    &mut vote_keys,
                    &mut dao_keys,
                    &mut event_keys,
                )?;
                events.push(proven);
            }
        }

        let last = blocks.last().ok_or(BuilderError::InvalidState)?;
        let end_block = last
            .block_number
            .checked_add(1)
            .ok_or(BuilderError::CursorInvalid)?;
        let final_block = proposal
            .end_block
            .checked_add(1)
            .ok_or(BuilderError::CursorInvalid)?;
        let candidate_since = if end_block == final_block {
            header_dep_index(&mut header_deps, last.block_hash)?;
            proposal.end_block
        } else {
            0
        };
        Ok(ScannedBatch {
            events,
            header_deps,
            end_block,
            end_tx_index: 0,
            candidate_since,
        })
    }

    pub fn scan_source_batch<S: ChainSource>(
        &self,
        proposal: &ProposalData,
        source: &S,
        through_block: u64,
    ) -> Result<ScannedBatch, ScanError<S::Error>> {
        if through_block < self.state.next_block || through_block > proposal.end_block {
            return Err(BuilderError::CursorInvalid.into());
        }
        let mut blocks = Vec::new();
        for block_number in self.state.next_block..=through_block {
            blocks.push(
                source
                    .block_by_number(block_number)
                    .map_err(ScanError::Source)?,
            );
        }
        self.scan_blocks(proposal, &blocks).map_err(Into::into)
    }

    pub fn build_batch(
        &mut self,
        proposal: &ProposalData,
        proven_events: Vec<ProvenTransaction>,
        end_block: u64,
        end_tx_index: u32,
        candidate_since: u64,
    ) -> Result<(TallyState, BatchWitness), BuilderError> {
        if self.state.phase != TallyPhase::Active
            || proven_events.len() > proposal.max_events_per_batch as usize
        {
            return Err(BuilderError::BatchLimit);
        }
        let final_cursor = (
            proposal
                .end_block
                .checked_add(1)
                .ok_or(BuilderError::CursorInvalid)?,
            0,
        );
        let input_cursor = (self.state.next_block, self.state.next_tx_index);
        let output_cursor = (end_block, end_tx_index);
        if output_cursor <= input_cursor || output_cursor > final_cursor {
            return Err(BuilderError::CursorInvalid);
        }

        let mut next = self.clone_builder();
        let mut vote_keys = BTreeSet::new();
        let mut dao_keys = BTreeSet::new();
        let mut event_keys = BTreeSet::new();
        let mut previous_cursor = None;
        for event in &proven_events {
            let cursor = (event.block_number, event.tx_index);
            if cursor < input_cursor
                || cursor >= output_cursor
                || event.block_number < proposal.start_block
                || event.block_number > proposal.end_block
                || previous_cursor.is_some_and(|previous| cursor <= previous)
            {
                return Err(BuilderError::InvalidEvent);
            }
            previous_cursor = Some(cursor);
            next.apply_event(
                proposal,
                event,
                &mut vote_keys,
                &mut dao_keys,
                &mut event_keys,
            )?;
        }
        if vote_keys.len() > proposal.max_state_keys_per_batch as usize
            || dao_keys.len() > proposal.max_state_keys_per_batch as usize
            || event_keys.len() > proposal.max_state_keys_per_batch as usize
        {
            return Err(BuilderError::BatchLimit);
        }

        let (
            vote_transitions,
            dao_transitions,
            event_transitions,
            vote_proof,
            dao_proof,
            event_proof,
            previous_vote_records,
        ) = if proven_events.is_empty() {
            (
                Vec::new(),
                Vec::new(),
                Vec::new(),
                Vec::new(),
                Vec::new(),
                Vec::new(),
                Vec::new(),
            )
        } else {
            let vote_transitions = transitions(&self.votes, &next.votes, &vote_keys)?;
            let dao_transitions = transitions(&self.dao, &next.dao, &dao_keys)?;
            let event_transitions = transitions(&self.events, &next.events, &event_keys)?;
            let vote_proof = compiled_proof(&self.votes, &vote_keys)?;
            let dao_proof = compiled_proof(&self.dao, &dao_keys)?;
            let event_proof = compiled_proof(&self.events, &event_keys)?;
            let previous_vote_records = vote_transitions
                .iter()
                .filter(|transition| transition.old_value != ZERO)
                .map(|transition| {
                    self.records
                        .get(&transition.key)
                        .cloned()
                        .ok_or(BuilderError::MissingStateKey)
                })
                .collect::<Result<Vec<_>, _>>()?;
            (
                vote_transitions,
                dao_transitions,
                event_transitions,
                vote_proof,
                dao_proof,
                event_proof,
                previous_vote_records,
            )
        };

        next.state.phase = if output_cursor == final_cursor {
            TallyPhase::Candidate
        } else {
            TallyPhase::Active
        };
        next.state.sequence = next
            .state
            .sequence
            .checked_add(1)
            .ok_or(BuilderError::TallyOverflow)?;
        if next.state.sequence > proposal.max_batch_sequence as u32 {
            return Err(BuilderError::BatchLimit);
        }
        next.state.next_block = end_block;
        next.state.next_tx_index = end_tx_index;
        next.state.processed_events = next
            .state
            .processed_events
            .checked_add(proven_events.len() as u64)
            .ok_or(BuilderError::TallyOverflow)?;
        next.state.votes_root = root(&next.votes);
        next.state.dao_root = root(&next.dao);
        next.state.events_root = root(&next.events);
        next.state.candidate_since = if next.state.phase == TallyPhase::Candidate {
            if candidate_since < proposal.end_block {
                return Err(BuilderError::CursorInvalid);
            }
            candidate_since
        } else {
            0
        };

        let witness = BatchWitness {
            end_block,
            end_tx_index,
            events: proven_events,
            previous_vote_records,
            vote_transitions,
            dao_transitions,
            event_transitions,
            vote_proof,
            dao_proof,
            event_proof,
        };
        if witness.encode().map_err(|_| BuilderError::Encoding)?.len()
            > proposal.max_batch_witness_bytes as usize
        {
            return Err(BuilderError::BatchLimit);
        }
        let output_state = next.state.clone();
        *self = next;
        Ok((output_state, witness))
    }

    pub fn build_omitted_vote_challenge(
        &self,
        challenger_lock_hash: Hash,
        omitted: ProvenTransaction,
    ) -> Result<treasury_common::TallyWitness, BuilderError> {
        let raw = RawTransaction::from_slice(&omitted.raw_transaction)
            .map_err(|_| BuilderError::InvalidEvent)?;
        let tx_hash: Hash = raw.calc_tx_hash().as_slice().try_into().unwrap();
        if value(&self.events, tx_hash)? != ZERO {
            return Err(BuilderError::InvalidEvent);
        }
        let keys = BTreeSet::from([tx_hash]);
        Ok(treasury_common::TallyWitness::ChallengeVote {
            challenger_lock_hash,
            omitted,
            event_proof: compiled_proof(&self.events, &keys)?,
        })
    }

    pub fn build_omitted_spend_challenge(
        &self,
        challenger_lock_hash: Hash,
        omitted_spend: ProvenTransaction,
        dao_out_point: OutPoint,
    ) -> Result<treasury_common::TallyWitness, BuilderError> {
        let raw = RawTransaction::from_slice(&omitted_spend.raw_transaction)
            .map_err(|_| BuilderError::InvalidEvent)?;
        if !raw
            .inputs()
            .into_iter()
            .any(|input| unpack_out_point(&input.previous_output()) == dao_out_point)
        {
            return Err(BuilderError::InvalidEvent);
        }
        let spend_hash: Hash = raw.calc_tx_hash().as_slice().try_into().unwrap();
        if value(&self.events, spend_hash)? != ZERO {
            return Err(BuilderError::InvalidEvent);
        }
        let dao_key = dao_out_point.key();
        let voter_lock_hash = value(&self.dao, dao_key)?;
        if voter_lock_hash == ZERO {
            return Err(BuilderError::InvalidState);
        }
        Ok(treasury_common::TallyWitness::ChallengeSpend {
            challenger_lock_hash,
            omitted_spend,
            dao_out_point,
            voter_lock_hash,
            event_proof: compiled_proof(&self.events, &BTreeSet::from([spend_hash]))?,
            dao_proof: compiled_proof(&self.dao, &BTreeSet::from([dao_key]))?,
        })
    }

    fn apply_event(
        &mut self,
        proposal: &ProposalData,
        event: &ProvenTransaction,
        vote_keys: &mut BTreeSet<Hash>,
        dao_keys: &mut BTreeSet<Hash>,
        event_keys: &mut BTreeSet<Hash>,
    ) -> Result<(), BuilderError> {
        let raw = RawTransaction::from_slice(&event.raw_transaction)
            .map_err(|_| BuilderError::InvalidEvent)?;
        let tx_hash: Hash = raw.calc_tx_hash().as_slice().try_into().unwrap();
        if value(&self.events, tx_hash)? != ZERO {
            return Err(BuilderError::InvalidEvent);
        }
        event_keys.insert(tx_hash);
        update(&mut self.events, tx_hash, EVENT_PRESENT)?;

        let spent_out_points = raw
            .inputs()
            .into_iter()
            .map(|input| unpack_out_point(&input.previous_output()))
            .collect::<Vec<_>>();
        let mut relevant = false;
        for out_point in &spent_out_points {
            let key = out_point.key();
            let voter = value(&self.dao, key)?;
            if voter != ZERO {
                self.remove_record(voter, vote_keys, dao_keys)?;
                relevant = true;
            }
        }
        for (index, output) in raw.outputs().into_iter().enumerate() {
            let Some(type_script) = output.type_().to_opt() else {
                continue;
            };
            if type_script.code_hash().as_slice() != proposal.vote_code_hash
                || type_script.hash_type().as_slice()[0] != proposal.vote_hash_type
                || type_script.args().raw_data().as_ref() != self.state.proposal_id
            {
                continue;
            }
            let vote_data = raw
                .outputs_data()
                .get(index)
                .ok_or(BuilderError::InvalidEvent)?
                .raw_data();
            let vote = VoteData::decode(&vote_data).map_err(|_| BuilderError::InvalidEvent)?;
            if vote.dao_dep_indices.len() > proposal.max_dao_deps_per_vote as usize {
                return Err(BuilderError::BatchLimit);
            }
            let voter_lock_hash = output
                .lock()
                .calc_script_hash()
                .as_slice()
                .try_into()
                .unwrap();
            let dao_out_points = vote
                .dao_dep_indices
                .into_iter()
                .map(|index| {
                    raw.cell_deps()
                        .get(index as usize)
                        .map(|dep| unpack_out_point(&dep.out_point()))
                        .ok_or(BuilderError::InvalidEvent)
                })
                .collect::<Result<Vec<_>, _>>()?;
            if dao_out_points
                .iter()
                .any(|out_point| spent_out_points.contains(out_point))
            {
                return Err(BuilderError::InvalidEvent);
            }
            self.apply_record(
                VoteRecord {
                    voter_lock_hash,
                    direction: vote.direction,
                    amount: vote.amount,
                    block_number: event.block_number,
                    tx_index: event.tx_index,
                    dao_out_points,
                },
                vote_keys,
                dao_keys,
            )?;
            relevant = true;
        }
        if relevant {
            Ok(())
        } else {
            Err(BuilderError::InvalidEvent)
        }
    }

    fn is_relevant(
        &self,
        proposal: &ProposalData,
        raw: &RawTransaction,
    ) -> Result<bool, BuilderError> {
        for input in raw.inputs().into_iter() {
            if value(&self.dao, unpack_out_point(&input.previous_output()).key())? != ZERO {
                return Ok(true);
            }
        }
        Ok(raw.outputs().into_iter().any(|output| {
            output.type_().to_opt().is_some_and(|script| {
                script.code_hash().as_slice() == proposal.vote_code_hash
                    && script.hash_type().as_slice()[0] == proposal.vote_hash_type
                    && script.args().raw_data().as_ref() == self.state.proposal_id
            })
        }))
    }

    fn remove_record(
        &mut self,
        voter: Hash,
        vote_keys: &mut BTreeSet<Hash>,
        dao_keys: &mut BTreeSet<Hash>,
    ) -> Result<(), BuilderError> {
        vote_keys.insert(voter);
        let record = self
            .records
            .remove(&voter)
            .ok_or(BuilderError::MissingStateKey)?;
        update(&mut self.votes, voter, ZERO)?;
        self.subtract_tally(record.direction, record.amount)?;
        for out_point in record.dao_out_points {
            let key = out_point.key();
            dao_keys.insert(key);
            if value(&self.dao, key)? != voter {
                return Err(BuilderError::InvalidState);
            }
            update(&mut self.dao, key, ZERO)?;
        }
        Ok(())
    }

    fn apply_record(
        &mut self,
        record: VoteRecord,
        vote_keys: &mut BTreeSet<Hash>,
        dao_keys: &mut BTreeSet<Hash>,
    ) -> Result<(), BuilderError> {
        let voter = record.voter_lock_hash;
        vote_keys.insert(voter);
        if value(&self.votes, voter)? != ZERO {
            self.remove_record(voter, vote_keys, dao_keys)?;
        }
        for out_point in &record.dao_out_points {
            let key = out_point.key();
            dao_keys.insert(key);
            if value(&self.dao, key)? != ZERO {
                return Err(BuilderError::InvalidState);
            }
            update(&mut self.dao, key, voter)?;
        }
        self.add_tally(record.direction, record.amount)?;
        let record_hash = record.value_hash().map_err(|_| BuilderError::Encoding)?;
        update(&mut self.votes, voter, record_hash)?;
        self.records.insert(voter, record);
        Ok(())
    }

    fn add_tally(&mut self, direction: u8, amount: u64) -> Result<(), BuilderError> {
        let target = if direction == 1 {
            &mut self.state.yes
        } else {
            &mut self.state.no
        };
        *target = target
            .checked_add(amount as u128)
            .ok_or(BuilderError::TallyOverflow)?;
        Ok(())
    }

    fn subtract_tally(&mut self, direction: u8, amount: u64) -> Result<(), BuilderError> {
        let target = if direction == 1 {
            &mut self.state.yes
        } else {
            &mut self.state.no
        };
        *target = target
            .checked_sub(amount as u128)
            .ok_or(BuilderError::InvalidState)?;
        Ok(())
    }

    fn clone_builder(&self) -> Self {
        Self {
            state: self.state.clone(),
            votes: clone_tree(&self.votes),
            dao: clone_tree(&self.dao),
            events: clone_tree(&self.events),
            records: self.records.clone(),
        }
    }
}

pub fn prove_transaction(
    block_number: u64,
    header_dep_index: u16,
    raw_transactions: &[Vec<u8>],
    witnesses_root: Hash,
    tx_index: u32,
) -> Result<ProvenTransaction, BuilderError> {
    if tx_index as usize >= raw_transactions.len() || raw_transactions.is_empty() {
        return Err(BuilderError::InvalidEvent);
    }
    let hashes = raw_transactions
        .iter()
        .map(|raw| {
            RawTransaction::from_slice(raw)
                .map(|tx| tx.calc_tx_hash().as_slice().try_into().unwrap())
                .map_err(|_| BuilderError::InvalidEvent)
        })
        .collect::<Result<Vec<Hash>, _>>()?;
    let proof = CBMT::<Hash, MergeHash>::build_merkle_proof(&hashes, &[tx_index])
        .ok_or(BuilderError::InvalidEvent)?;
    Ok(ProvenTransaction {
        block_number,
        header_dep_index,
        tx_index,
        tx_count: hashes.len() as u32,
        raw_transaction: raw_transactions[tx_index as usize].clone(),
        witnesses_root,
        lemmas: proof.lemmas().to_vec(),
    })
}

fn transitions(
    old: &Smt,
    new: &Smt,
    keys: &BTreeSet<Hash>,
) -> Result<Vec<LeafTransition>, BuilderError> {
    keys.iter()
        .map(|key| {
            Ok(LeafTransition {
                key: *key,
                old_value: value(old, *key)?,
                new_value: value(new, *key)?,
            })
        })
        .collect()
}

fn compiled_proof(tree: &Smt, keys: &BTreeSet<Hash>) -> Result<Vec<u8>, BuilderError> {
    if keys.is_empty() {
        return Err(BuilderError::MissingStateKey);
    }
    let keys = keys.iter().copied().map(H256::from).collect::<Vec<_>>();
    tree.merkle_proof(keys.clone())
        .and_then(|proof| proof.compile(keys))
        .map(|proof| proof.0)
        .map_err(|_| BuilderError::Smt)
}

fn value(tree: &Smt, key: Hash) -> Result<Hash, BuilderError> {
    tree.get(&H256::from(key))
        .map(Into::into)
        .map_err(|_| BuilderError::Smt)
}

fn update(tree: &mut Smt, key: Hash, value: Hash) -> Result<(), BuilderError> {
    tree.update(H256::from(key), H256::from(value))
        .map(|_| ())
        .map_err(|_| BuilderError::Smt)
}

fn root(tree: &Smt) -> Hash {
    (*tree.root()).into()
}

fn clone_tree(tree: &Smt) -> Smt {
    Smt::new(*tree.root(), tree.store().clone())
}

fn unpack_out_point(out_point: &ckb_gen_types::packed::OutPoint) -> OutPoint {
    OutPoint {
        tx_hash: out_point.tx_hash().as_slice().try_into().unwrap(),
        index: out_point.index().unpack(),
    }
}

fn header_dep_index(header_deps: &mut Vec<Hash>, block_hash: Hash) -> Result<u16, BuilderError> {
    if let Some(index) = header_deps.iter().position(|hash| *hash == block_hash) {
        return index.try_into().map_err(|_| BuilderError::BatchLimit);
    }
    let index = header_deps
        .len()
        .try_into()
        .map_err(|_| BuilderError::BatchLimit)?;
    header_deps.push(block_hash);
    Ok(index)
}

#[cfg(test)]
mod tests {
    use super::*;
    use ckb_gen_types::bytes::Bytes;
    use ckb_gen_types::packed;
    use treasury_common::{ProposalPhase, verify_cbmt_inclusion, verify_smt_transition};

    fn proposal() -> ProposalData {
        ProposalData {
            phase: ProposalPhase::Closed,
            start_block: 10,
            end_block: 20,
            challenge_period: 5,
            max_events_per_batch: 100,
            max_dao_deps_per_vote: 64,
            max_state_keys_per_batch: 4096,
            max_batch_sequence: 128,
            max_batch_witness_bytes: 500_000,
            minimum_vote_capacity: 1,
            requested_amount: 1000,
            receiver_lock_hash: [1; 32],
            dao_code_hash: [9; 32],
            dao_hash_type: 1,
            vote_code_hash: [2; 32],
            vote_hash_type: 1,
            tally_code_hash: [3; 32],
            tally_hash_type: 1,
            policy_type_hash: [4; 32],
            metadata_hash: [5; 32],
        }
    }

    fn raw_transaction(nonce: u32) -> Vec<u8> {
        packed::RawTransaction::new_builder()
            .version(nonce)
            .build()
            .as_slice()
            .to_vec()
    }

    fn vote_and_spend_transactions(proposal_id: Hash) -> (Vec<u8>, Vec<u8>) {
        let dao_out_point = packed::OutPoint::new_builder()
            .tx_hash([9u8; 32].pack())
            .index(0u32)
            .build();
        let vote_type = packed::Script::new_builder()
            .code_hash([2u8; 32].pack())
            .hash_type(1u8)
            .args(Bytes::from(proposal_id.to_vec()).pack())
            .build();
        let vote_data = VoteData {
            direction: 1,
            amount: 100,
            dao_dep_indices: vec![0],
        }
        .encode()
        .unwrap();
        let vote = packed::RawTransaction::new_builder()
            .cell_deps(
                [packed::CellDep::new_builder()
                    .out_point(dao_out_point.clone())
                    .build()]
                .pack(),
            )
            .outputs(
                [packed::CellOutput::new_builder()
                    .type_(Some(vote_type).pack())
                    .build()]
                .pack(),
            )
            .outputs_data([Bytes::from(vote_data)].pack())
            .build();
        let spend = packed::RawTransaction::new_builder()
            .inputs(
                [packed::CellInput::new_builder()
                    .previous_output(dao_out_point)
                    .build()]
                .pack(),
            )
            .build();
        (vote.as_slice().to_vec(), spend.as_slice().to_vec())
    }

    #[test]
    fn builds_transaction_inclusion_proof_compatible_with_contract_verifier() {
        let raws = vec![raw_transaction(0), raw_transaction(1), raw_transaction(2)];
        let proven = prove_transaction(10, 0, &raws, [9; 32], 1).unwrap();
        let hashes = raws
            .iter()
            .map(|raw| {
                RawTransaction::from_slice(raw)
                    .unwrap()
                    .calc_tx_hash()
                    .as_slice()
                    .try_into()
                    .unwrap()
            })
            .collect::<Vec<Hash>>();
        let root = CBMT::<Hash, MergeHash>::build_merkle_root(&hashes);
        assert!(verify_cbmt_inclusion(hashes[1], 1, 3, &proven.lemmas, root));
    }

    #[test]
    fn generated_smt_transition_verifies_with_shared_contract_logic() {
        let mut old = Smt::default();
        let key = [1; 32];
        update(&mut old, key, [2; 32]).unwrap();
        let mut new = clone_tree(&old);
        update(&mut new, key, [3; 32]).unwrap();
        let keys = BTreeSet::from([key]);
        let transition = transitions(&old, &new, &keys).unwrap();
        let proof = compiled_proof(&old, &keys).unwrap();
        assert!(verify_smt_transition(
            root(&old),
            root(&new),
            &proof,
            &transition
        ));
    }

    #[test]
    fn new_builder_matches_proposal_start() {
        let proposal = proposal();
        let builder = TallyBuilder::new([7; 32], [8; 32], proposal.start_block);
        assert_eq!(builder.state().next_block, proposal.start_block);
        assert_eq!(builder.state().votes_root, ZERO);
    }

    #[test]
    fn empty_final_batch_preserves_roots_and_tally() {
        let proposal = proposal();
        let mut builder = TallyBuilder::new([7; 32], [8; 32], proposal.start_block);
        let (state, batch) = builder
            .build_batch(
                &proposal,
                Vec::new(),
                proposal.end_block + 1,
                0,
                proposal.end_block,
            )
            .unwrap();
        assert_eq!(state.phase, TallyPhase::Candidate);
        assert_eq!(state.votes_root, ZERO);
        assert_eq!(state.yes, 0);
        assert!(batch.vote_transitions.is_empty());
        assert!(batch.vote_proof.is_empty());
    }

    #[test]
    fn scanner_carries_vote_state_across_blocks() {
        let mut proposal = proposal();
        proposal.end_block = 11;
        let proposal_id = [7; 32];
        let builder = TallyBuilder::new(proposal_id, [8; 32], proposal.start_block);
        let (vote, spend) = vote_and_spend_transactions(proposal_id);
        let scanned = builder
            .scan_blocks(
                &proposal,
                &[
                    ChainBlock {
                        block_number: 10,
                        block_hash: [10; 32],
                        parent_hash: [9; 32],
                        raw_transactions: vec![vote],
                        witnesses_root: [20; 32],
                    },
                    ChainBlock {
                        block_number: 11,
                        block_hash: [11; 32],
                        parent_hash: [10; 32],
                        raw_transactions: vec![spend],
                        witnesses_root: [21; 32],
                    },
                ],
            )
            .unwrap();
        assert_eq!(scanned.events.len(), 2);
        assert_eq!(scanned.events[1].header_dep_index, 1);
        assert_eq!(scanned.header_deps, vec![[10; 32], [11; 32]]);
        assert_eq!((scanned.end_block, scanned.end_tx_index), (12, 0));

        let mut builder = builder;
        let (candidate, _) = builder
            .build_batch(
                &proposal,
                scanned.events,
                scanned.end_block,
                scanned.end_tx_index,
                scanned.candidate_since,
            )
            .unwrap();
        assert_eq!(candidate.phase, TallyPhase::Candidate);
        assert_eq!(candidate.yes, 0);
    }

    #[test]
    fn spend_challenge_requires_candidate_to_claim_dao_is_live() {
        let mut proposal = proposal();
        proposal.end_block = 11;
        let proposal_id = [7; 32];
        let (vote, spend) = vote_and_spend_transactions(proposal_id);
        let vote = prove_transaction(10, 0, &[vote], [20; 32], 0).unwrap();
        let spend = prove_transaction(11, 1, &[spend], [21; 32], 0).unwrap();
        let dao_out_point = OutPoint {
            tx_hash: [9; 32],
            index: 0,
        };

        let mut omitted_builder = TallyBuilder::new(proposal_id, [8; 32], proposal.start_block);
        omitted_builder
            .build_batch(
                &proposal,
                vec![vote.clone()],
                proposal.end_block + 1,
                0,
                proposal.end_block,
            )
            .unwrap();
        assert!(
            omitted_builder
                .build_omitted_spend_challenge([6; 32], spend.clone(), dao_out_point)
                .is_ok()
        );

        let mut complete_builder = TallyBuilder::new(proposal_id, [8; 32], proposal.start_block);
        complete_builder
            .build_batch(
                &proposal,
                vec![vote, spend.clone()],
                proposal.end_block + 1,
                0,
                proposal.end_block,
            )
            .unwrap();
        assert!(
            complete_builder
                .build_omitted_spend_challenge([6; 32], spend, dao_out_point)
                .is_err()
        );
    }
}
