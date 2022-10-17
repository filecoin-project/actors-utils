use fvm_shared::ActorID;

pub trait OperatorSet {
    fn add_operator(&mut self, operator: ActorID);
    fn remove_operator(&mut self, operator: &ActorID);
    fn contains_actor(&self, operator: &ActorID) -> bool;
}

/// Maintains set-like invariants in-memory by maintaining sorted order of the underlying array
/// Insertion and deletion are O(n) operations but we expect operator lists to be a relatively small size
/// TODO: benchmark this against some other options such as
/// - BTreeSet in memory, Vec serialized
/// - BTreeSet in memory and serialization
/// - HashSets...
/// - Hamt<ActorID, ()>
/// - Amt<ActorID>
impl OperatorSet for Vec<ActorID> {
    /// Attempts to add the operator to the authorised list
    ///
    /// Returns true if the operator was added, false if it was already present
    fn add_operator(&mut self, id: ActorID) {
        if let Err(pos) = self.binary_search(&id) {
            self.insert(pos, id);
        }
    }

    /// Removes the operator from the authorised list
    fn remove_operator(&mut self, id: &ActorID) {
        if let Ok(pos) = self.binary_search(id) {
            self.remove(pos);
        }
    }

    /// Checks if the operator is present in the list
    fn contains_actor(&self, id: &ActorID) -> bool {
        self.binary_search(id).is_ok()
    }
}

#[cfg(test)]
mod test {
    use fvm_shared::ActorID;

    use super::OperatorSet;

    #[test]
    fn test_idempotent_add() {
        let mut operators: Vec<ActorID> = vec![];
        operators.add_operator(1);
        operators.add_operator(1);
        operators.add_operator(1);

        assert!(operators.contains_actor(&1));
        assert!(operators.len() == 1);

        operators.add_operator(2);
        operators.add_operator(2);
        operators.add_operator(1);
        assert!(operators.contains_actor(&1));
        assert!(operators.contains_actor(&2));
        assert!(operators.len() == 2);
    }

    #[test]
    fn test_ordered_add() {
        let mut operators: Vec<ActorID> = vec![];
        operators.add_operator(2);
        operators.add_operator(3);
        operators.add_operator(5);
        operators.add_operator(1);
        operators.add_operator(4);

        assert!(operators.len() == 5);
        assert_eq!(operators, vec![1, 2, 3, 4, 5]);
    }

    #[test]
    fn test_removal() {
        let mut operators: Vec<ActorID> = vec![];
        operators.add_operator(2);
        operators.add_operator(3);
        operators.add_operator(5);
        operators.add_operator(1);
        operators.add_operator(4);

        operators.remove_operator(&2);
        operators.remove_operator(&2);

        assert!(operators.len() == 4);
        assert_eq!(operators, vec![1, 3, 4, 5]);

        operators.remove_operator(&4);
        operators.remove_operator(&4);
        assert!(operators.len() == 3);
        assert_eq!(operators, vec![1, 3, 5]);
    }
}
