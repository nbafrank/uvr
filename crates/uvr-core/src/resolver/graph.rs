use std::collections::{HashMap, HashSet, VecDeque};

use crate::error::{Result, UvrError};

/// A simple directed dependency graph for topological ordering.
#[derive(Debug, Default)]
pub struct DependencyGraph {
    /// node → its dependencies (edges: node → dep)
    edges: HashMap<String, Vec<String>>,
}

impl DependencyGraph {
    pub fn add_node(&mut self, name: &str) {
        self.edges.entry(name.to_string()).or_default();
    }

    pub fn add_edge(&mut self, from: &str, to: &str) {
        self.edges.entry(from.to_string()).or_default().push(to.to_string());
        self.edges.entry(to.to_string()).or_default();
    }

    /// Kahn's algorithm — returns nodes in install order (deps first).
    pub fn topological_sort(&self) -> Result<Vec<String>> {
        // in-degree count
        let mut in_degree: HashMap<String, usize> = self.edges.keys().map(|k| (k.clone(), 0)).collect();
        for deps in self.edges.values() {
            for dep in deps {
                *in_degree.entry(dep.clone()).or_insert(0) += 1;
            }
        }

        let mut queue: VecDeque<String> = in_degree
            .iter()
            .filter(|(_, &d)| d == 0)
            .map(|(k, _)| k.clone())
            .collect();
        // deterministic order
        let mut sorted_queue: Vec<String> = queue.drain(..).collect();
        sorted_queue.sort();
        let mut queue: VecDeque<String> = sorted_queue.into();

        let mut result = Vec::new();
        while let Some(node) = queue.pop_front() {
            result.push(node.clone());
            let mut next: Vec<String> = self
                .edges
                .get(&node)
                .cloned()
                .unwrap_or_default()
                .into_iter()
                .filter_map(|dep| {
                    let d = in_degree.get_mut(&dep)?;
                    *d -= 1;
                    if *d == 0 { Some(dep) } else { None }
                })
                .collect();
            next.sort();
            queue.extend(next);
        }

        if result.len() != self.edges.len() {
            let visited: HashSet<_> = result.iter().cloned().collect();
            let cycle_nodes: Vec<_> = self.edges.keys().filter(|k| !visited.contains(*k)).cloned().collect();
            return Err(UvrError::CircularDependency(cycle_nodes.join(", ")));
        }

        // Edges are "dependent → dependency", so Kahn's processes dependents first.
        // Reverse to get install order (dependencies first).
        result.reverse();
        Ok(result)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn simple_topo() {
        let mut g = DependencyGraph::default();
        // ggplot2 depends on dplyr, dplyr depends on rlang
        g.add_edge("ggplot2", "dplyr");
        g.add_edge("dplyr", "rlang");
        g.add_node("rlang");

        // In Kahn with "from → dep" edges meaning "from needs dep":
        // in-degree for "dep" increases. So rlang has highest in-degree.
        // But install order should be: rlang first, then dplyr, then ggplot2.
        let order = g.topological_sort().unwrap();
        let pos = |name: &str| order.iter().position(|x| x == name).unwrap();
        assert!(pos("rlang") < pos("dplyr"));
        assert!(pos("dplyr") < pos("ggplot2"));
    }

    #[test]
    fn cycle_detected() {
        let mut g = DependencyGraph::default();
        g.add_edge("a", "b");
        g.add_edge("b", "a");
        assert!(g.topological_sort().is_err());
    }
}
