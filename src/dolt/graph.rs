use std::collections::{BTreeMap, HashMap};

#[derive(Default)]
pub(super) struct Graph<'a>(BTreeMap<&'a str, Vec<&'a str>>);

impl<'a> Graph<'a> {
    pub(super) fn add(&mut self, from: &'a str, to: &'a str) {
        self.0.entry(from).or_default().push(to);
    }

    pub(super) fn extend(&mut self, other: Self) {
        for (from, to) in other.0 {
            self.0.entry(from).or_default().extend(to);
        }
    }

    fn neighbors(&self, node: &str) -> &[&'a str] {
        self.0.get(node).map_or(&[], Vec::as_slice)
    }

    pub(super) fn cycle(&self) -> Option<Vec<&'a str>> {
        #[derive(Clone, Copy)]
        enum Visit {
            Active,
            Complete,
        }

        let mut visits = HashMap::new();
        let mut stack = Vec::new();
        for &start in self.0.keys() {
            if visits.contains_key(start) {
                continue;
            }
            visits.insert(start, Visit::Active);
            stack.push((start, self.neighbors(start).iter()));
            while let Some((node, neighbors)) = stack.last_mut() {
                match neighbors.next().copied() {
                    Some(next) => match visits.get(next) {
                        Some(Visit::Active) => {
                            return Some(
                                stack
                                    .iter()
                                    .skip_while(|(node, _)| *node != next)
                                    .map(|(node, _)| *node)
                                    .take(16)
                                    .collect(),
                            );
                        }
                        Some(Visit::Complete) => {}
                        None => {
                            visits.insert(next, Visit::Active);
                            stack.push((next, self.neighbors(next).iter()));
                        }
                    },
                    None => {
                        visits.insert(*node, Visit::Complete);
                        stack.pop();
                    }
                }
            }
        }
        None
    }
}

#[cfg(test)]
mod tests {
    use super::Graph;

    #[test]
    fn shared_prerequisite_is_acyclic_until_a_back_edge_is_added() {
        let mut graph = Graph::default();
        graph.add("a", "b");
        graph.add("a", "c");
        graph.add("b", "d");
        graph.add("c", "d");
        assert_eq!(graph.cycle(), None);

        graph.add("d", "b");
        assert_eq!(graph.cycle(), Some(vec!["b", "d"]));
    }

    #[test]
    fn deep_graph_is_iterative_and_cycle_witness_is_bounded() {
        let ids: Vec<_> = (0..20_000).map(|number| format!("{number:05}")).collect();
        let mut graph = Graph::default();
        for edge in ids.windows(2) {
            graph.add(&edge[0], &edge[1]);
        }
        assert_eq!(graph.cycle(), None);

        graph.add(ids.last().unwrap(), &ids[0]);
        let cycle = graph.cycle().unwrap();
        assert_eq!(
            cycle,
            ids[..16].iter().map(String::as_str).collect::<Vec<_>>()
        );
    }
}
