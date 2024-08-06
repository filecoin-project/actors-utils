use fvm_ipld_bitfield::BitField;
use fvm_shared::ActorID;

pub trait OperatorSet {
    /// Attempts to add the operator to the authorised list.
    ///
    /// Returns true if the operator was added, false if it was already present.
    fn add_operator(&mut self, operator: ActorID);

    /// Removes the operator from the authorised list.
    fn remove_operator(&mut self, operator: &ActorID);

    /// Checks if the operator is present in the list.
    fn contains_actor(&self, operator: &ActorID) -> bool;
}

impl OperatorSet for BitField {
    fn add_operator(&mut self, operator: ActorID) {
        self.set(operator);
    }

    fn remove_operator(&mut self, operator: &ActorID) {
        self.unset(*operator);
    }

    fn contains_actor(&self, operator: &ActorID) -> bool {
        self.get(*operator)
    }
}

// TODO: benchmark this against some other options such as
// - BTreeSet in memory, Vec serialized
// - BTreeSet in memory and serialization
// - HashSets...
// - Hamt<ActorID, ()>
// - Amt<ActorID>

/// Maintains set-like invariants in-memory by maintaining sorted order of the underlying array.
///
/// Insertion and deletion are O(n) operations but we expect operator lists to be a relatively small
/// size.
impl OperatorSet for Vec<ActorID> {
    fn add_operator(&mut self, id: ActorID) {
        if let Err(pos) = self.binary_search(&id) {
            self.insert(pos, id);
        }
    }

    fn remove_operator(&mut self, id: &ActorID) {
        if let Ok(pos) = self.binary_search(id) {
            self.remove(pos);
        }
    }

    fn contains_actor(&self, id: &ActorID) -> bool {
        self.binary_search(id).is_ok()
    }
}

#[cfg(test)]
mod vec_test {
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

#[cfg(test)]
mod bitfield_test {
    use fvm_ipld_bitfield::BitField;

    use super::OperatorSet;

    #[test]
    fn test_idempotent_add() {
        let mut operators: BitField = BitField::default();
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
        let mut operators: BitField = BitField::default();
        operators.add_operator(2);
        operators.add_operator(3);
        operators.add_operator(5);
        operators.add_operator(1);
        operators.add_operator(4);

        assert!(operators.len() == 5);
        assert!(operators.get(1));
        assert!(operators.get(2));
        assert!(operators.get(3));
        assert!(operators.get(4));
        assert!(operators.get(5));
    }

    #[test]
    fn test_removal() {
        let mut operators: BitField = BitField::default();
        operators.add_operator(2);
        operators.add_operator(3);
        operators.add_operator(5);
        operators.add_operator(1);
        operators.add_operator(4);

        operators.remove_operator(&2);
        operators.remove_operator(&2);

        assert!(operators.len() == 4);
        assert!(operators.get(1));
        assert!(!operators.get(2));
        assert!(operators.get(3));
        assert!(operators.get(4));
        assert!(operators.get(5));

        operators.remove_operator(&4);

        assert!(operators.len() == 3);
        assert!(operators.get(1));
        assert!(!operators.get(2));
        assert!(operators.get(3));
        assert!(!operators.get(4));
        assert!(operators.get(5));
    }
}
