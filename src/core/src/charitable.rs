//! Charitable vote scaffolding (#47)
use std::collections::HashMap;
use serde::{Deserialize, Serialize};
use crate::identity::Address;
use crate::token::Amount;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub enum CharitableCategory { FeedHungry, CureDisease, ImproveEnvironment, ProvideHealthcare, HouseHomeless, MentalHealth, RehabilitateAddicted, Education, ElderlyCare, AnimalShelters, DisabilityAssistance, FundCivilServants }

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct CharitableProposal { pub id: u64, pub title: String, pub description: String, pub recipient_hash: [u8; 32], pub category: CharitableCategory, pub proposed_amount: Amount }

#[derive(Debug, Clone)]
pub struct CharitableVoteState { pub proposals: Vec<CharitableProposal>, pub votes: HashMap<Address, u64>, pub epoch: u64 }

impl CharitableVoteState { pub fn new(epoch: u64) -> Self { Self { proposals: Vec::new(), votes: HashMap::new(), epoch } } }

pub fn cast_vote(state: &mut CharitableVoteState, voter: Address, proposal_id: u64) -> Result<(), String> {
    if !state.proposals.iter().any(|p| p.id == proposal_id) { return Err(format!("Proposal {} does not exist", proposal_id)); }
    state.votes.insert(voter, proposal_id);
    Ok(())
}

pub fn tally_votes(state: &CharitableVoteState) -> Vec<(u64, u64)> {
    let mut counts: HashMap<u64, u64> = HashMap::new();
    for &pid in state.votes.values() { *counts.entry(pid).or_insert(0) += 1; }
    let mut results: Vec<(u64, u64)> = counts.into_iter().collect();
    results.sort_by(|a, b| b.1.cmp(&a.1).then(a.0.cmp(&b.0)));
    results
}

#[cfg(test)]
mod tests {
    use super::*;
    fn prop(id: u64, cat: CharitableCategory) -> CharitableProposal { CharitableProposal { id, title: format!("P{}", id), description: "test".into(), recipient_hash: [id as u8; 32], category: cat, proposed_amount: Amount::from_comme(1000) } }
    fn addr(n: u8) -> Address { Address([n; 32]) }
    #[test] fn test_cast_ok() { let mut s = CharitableVoteState::new(1); s.proposals.push(prop(1, CharitableCategory::FeedHungry)); assert!(cast_vote(&mut s, addr(1), 1).is_ok()); }
    #[test] fn test_cast_bad() { let mut s = CharitableVoteState::new(1); s.proposals.push(prop(1, CharitableCategory::FeedHungry)); assert!(cast_vote(&mut s, addr(1), 99).is_err()); }
    #[test] fn test_change_vote() { let mut s = CharitableVoteState::new(1); s.proposals.push(prop(1, CharitableCategory::FeedHungry)); s.proposals.push(prop(2, CharitableCategory::Education)); cast_vote(&mut s, addr(1), 1).unwrap(); cast_vote(&mut s, addr(1), 2).unwrap(); assert_eq!(s.votes[&addr(1)], 2); }
    #[test] fn test_tally() { let mut s = CharitableVoteState::new(1); s.proposals.push(prop(1, CharitableCategory::FeedHungry)); s.proposals.push(prop(2, CharitableCategory::Education)); cast_vote(&mut s, addr(1), 2).unwrap(); cast_vote(&mut s, addr(2), 2).unwrap(); cast_vote(&mut s, addr(3), 1).unwrap(); let r = tally_votes(&s); assert_eq!(r[0], (2, 2)); assert_eq!(r[1], (1, 1)); }
    #[test] fn test_tally_empty() { assert!(tally_votes(&CharitableVoteState::new(1)).is_empty()); }
    #[test] fn test_categories() { let cats = [CharitableCategory::FeedHungry, CharitableCategory::CureDisease, CharitableCategory::ImproveEnvironment, CharitableCategory::ProvideHealthcare, CharitableCategory::HouseHomeless, CharitableCategory::MentalHealth, CharitableCategory::RehabilitateAddicted, CharitableCategory::Education, CharitableCategory::ElderlyCare, CharitableCategory::AnimalShelters, CharitableCategory::DisabilityAssistance, CharitableCategory::FundCivilServants]; assert_eq!(cats.len(), 12); }
}
