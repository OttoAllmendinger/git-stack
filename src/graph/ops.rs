pub use crate::graph::Node;

pub fn protect_branches(
    root: &mut Node,
    repo: &dyn crate::git::Repo,
    protected_branches: &crate::git::Branches,
) -> Result<(), git2::Error> {
    // Assuming the root is the base.  The base is not guaranteed to be a protected branch but
    // might be an ancestor of one.
    for protected_oid in protected_branches.oids() {
        if let Some(merge_base_oid) = repo.merge_base(root.local_commit.id, protected_oid) {
            if merge_base_oid == root.local_commit.id {
                root.action = crate::graph::Action::Protected;
                break;
            }
        }
    }

    for stack in root.stacks.iter_mut() {
        protect_branches_internal(stack, repo, protected_branches)?;
    }

    Ok(())
}

fn protect_branches_internal(
    nodes: &mut Vec<Node>,
    repo: &dyn crate::git::Repo,
    protected_branches: &crate::git::Branches,
) -> Result<bool, git2::Error> {
    let mut descendant_protected = false;
    for node in nodes.iter_mut().rev() {
        let mut stacks_protected = false;
        for stack in node.stacks.iter_mut() {
            let stack_protected = protect_branches_internal(stack, repo, protected_branches)?;
            stacks_protected |= stack_protected;
        }
        let self_protected = protected_branches.contains_oid(node.local_commit.id);
        if descendant_protected || stacks_protected || self_protected {
            node.action = crate::graph::Action::Protected;
            descendant_protected = true;
        }
    }

    Ok(descendant_protected)
}

pub fn rebase_branches(node: &mut Node, new_base: git2::Oid) -> Result<(), git2::Error> {
    rebase_branches_internal(node, new_base)?;

    Ok(())
}

/// Mark a new base commit for the last protected commit on each branch.
fn rebase_branches_internal(node: &mut Node, new_base: git2::Oid) -> Result<bool, git2::Error> {
    if !node.stacks.is_empty() {
        let mut all_stacks_rebased = true;
        for stack in node.stacks.iter_mut() {
            let mut stack_rebased = false;
            for node in stack.iter_mut().rev() {
                let node_rebase = rebase_branches_internal(node, new_base)?;
                if node_rebase {
                    stack_rebased = true;
                    break;
                }
            }
            if !stack_rebased {
                all_stacks_rebased = false;
            }
        }

        if all_stacks_rebased {
            return Ok(true);
        }
    }

    if node.local_commit.id == new_base {
        Ok(true)
    } else if node.action == crate::graph::Action::Protected {
        node.action = crate::graph::Action::Rebase(new_base);
        Ok(true)
    } else {
        Ok(false)
    }
}

pub fn pushable(node: &mut Node) -> Result<(), git2::Error> {
    if node.action.is_protected() || node.action.is_rebase() || node.branches.is_empty() {
        for stack in node.stacks.iter_mut() {
            pushable_stack(stack)?;
        }
    }
    Ok(())
}

fn pushable_stack(nodes: &mut [Node]) -> Result<(), git2::Error> {
    let mut cause = None;
    for node in nodes.iter_mut() {
        if node.action.is_protected() || node.action.is_rebase() {
            assert_eq!(cause, None);
            for stack in node.stacks.iter_mut() {
                pushable_stack(stack)?;
            }
            continue;
        }

        if node.local_commit.wip_summary().is_some() {
            cause = Some("contains WIP commit");
        }

        if !node.branches.is_empty() {
            let branch = &node.branches[0];
            if let Some(cause) = cause {
                log::debug!("{} isn't pushable, {}", branch.name, cause);
            } else if node.branches.iter().all(|b| Some(b.id) == b.push_id) {
                log::debug!("{} is already pushed", branch.name);
            } else {
                log::debug!("{} is pushable", branch.name);
                node.pushable = true;
            }
            return Ok(());
        } else if !node.stacks.is_empty() {
            cause = Some("ambiguous which branch owns some commits");
        }
    }

    Ok(())
}

pub fn drop_by_tree_id(
    node: &mut Node,
    onto: &[std::rc::Rc<crate::git::Commit>],
) -> Result<(), git2::Error> {
    if node.action.is_protected() || node.action.is_rebase() {
        let tree_ids: std::collections::HashSet<_> = onto.iter().map(|c| c.tree_id).collect();

        let mut moved_stacks = Vec::new();
        for stack in node.stacks.iter_mut() {
            moved_stacks.extend(stack_drop_by_tree_id(stack, &tree_ids)?);
        }
        node.stacks.extend(moved_stacks);
    }
    Ok(())
}

fn stack_drop_by_tree_id(
    nodes: &mut [Node],
    onto: &std::collections::HashSet<git2::Oid>,
) -> Result<Vec<Vec<Node>>, git2::Error> {
    let mut moved_stacks = Vec::new();
    let mut last_protected = None;
    for (i, node) in nodes.iter_mut().enumerate() {
        match node.action {
            crate::graph::Action::Protected | crate::graph::Action::Rebase(_) => {
                last_protected = Some(i);
                for stack in node.stacks.iter_mut() {
                    moved_stacks.extend(stack_drop_by_tree_id(stack, onto)?);
                }
            }
            crate::graph::Action::Pick => {
                if onto.contains(&node.local_commit.tree_id) {
                    node.action = crate::graph::Action::Delete;
                    let mut node_stacks = Vec::new();
                    std::mem::swap(&mut node.stacks, &mut node_stacks);
                    moved_stacks.extend(node_stacks);
                } else {
                    break;
                }
            }
            crate::graph::Action::Delete => {
                break;
            }
        }
    }

    if let Some(last_protected) = last_protected {
        nodes[last_protected].stacks.extend(moved_stacks);
        moved_stacks = Vec::new();
    }

    Ok(moved_stacks)
}

pub fn delinearize(node: &mut Node) {
    for stack in node.stacks.iter_mut() {
        delinearize_internal(stack);
    }
}

fn delinearize_internal(nodes: &mut Vec<Node>) {
    for node in nodes.iter_mut() {
        for stack in node.stacks.iter_mut() {
            delinearize_internal(stack);
        }
    }

    let splits: Vec<_> = nodes
        .iter()
        .enumerate()
        .filter(|(_, n)| !n.branches.is_empty())
        .map(|(i, _)| i + 1)
        .rev()
        .collect();
    for split in splits {
        if split == nodes.len() {
            continue;
        }
        let stack = nodes.split_off(split);
        assert!(!stack.is_empty());
        nodes.last_mut().unwrap().stacks.push(stack);
    }
}

pub fn to_script(node: &Node) -> crate::git::Script {
    let mut script = crate::git::Script::new();

    match node.action {
        crate::graph::Action::Pick => {
            // The base should be immutable, so nothing to cherry-pick
            let stack_mark = node.local_commit.id;
            script
                .commands
                .push(crate::git::Command::SwitchCommit(stack_mark));
            script
                .commands
                .push(crate::git::Command::RegisterMark(stack_mark));
            for stack in node.stacks.iter() {
                script
                    .dependents
                    .extend(to_script_internal(stack, node.local_commit.id));
            }
        }
        crate::graph::Action::Protected => {
            let stack_mark = node.local_commit.id;
            script
                .commands
                .push(crate::git::Command::SwitchCommit(stack_mark));
            script
                .commands
                .push(crate::git::Command::RegisterMark(stack_mark));
            for stack in node.stacks.iter() {
                script
                    .dependents
                    .extend(to_script_internal(stack, node.local_commit.id));
            }
        }
        crate::graph::Action::Rebase(new_base) => {
            script
                .commands
                .push(crate::git::Command::SwitchCommit(new_base));
            script
                .commands
                .push(crate::git::Command::RegisterMark(new_base));
            for stack in node.stacks.iter() {
                script
                    .dependents
                    .extend(to_script_internal(stack, new_base));
            }
        }
        crate::graph::Action::Delete => {
            assert!(node.stacks.is_empty());
            for branch in node.branches.iter() {
                script
                    .commands
                    .push(crate::git::Command::DeleteBranch(branch.name.clone()));
            }
        }
    }

    script
}

fn to_script_internal(nodes: &[Node], base_mark: git2::Oid) -> Option<crate::git::Script> {
    let mut script = crate::git::Script::new();
    for node in nodes {
        match node.action {
            crate::graph::Action::Pick => {
                script
                    .commands
                    .push(crate::git::Command::CherryPick(node.local_commit.id));
                for branch in node.branches.iter() {
                    script
                        .commands
                        .push(crate::git::Command::CreateBranch(branch.name.clone()));
                }

                if !node.stacks.is_empty() {
                    let stack_mark = node.local_commit.id;
                    script
                        .commands
                        .push(crate::git::Command::RegisterMark(stack_mark));
                    for stack in node.stacks.iter() {
                        script
                            .dependents
                            .extend(to_script_internal(stack, stack_mark));
                    }
                }
            }
            crate::graph::Action::Protected => {
                for stack in node.stacks.iter() {
                    script
                        .commands
                        .push(crate::git::Command::RegisterMark(node.local_commit.id));
                    script
                        .dependents
                        .extend(to_script_internal(stack, node.local_commit.id));
                }
            }
            crate::graph::Action::Rebase(new_base) => {
                script
                    .commands
                    .push(crate::git::Command::SwitchCommit(new_base));
                script
                    .commands
                    .push(crate::git::Command::RegisterMark(new_base));
                for stack in node.stacks.iter() {
                    script
                        .dependents
                        .extend(to_script_internal(stack, new_base));
                }
            }
            crate::graph::Action::Delete => {
                assert!(node.stacks.is_empty());
                for branch in node.branches.iter() {
                    script
                        .commands
                        .push(crate::git::Command::DeleteBranch(branch.name.clone()));
                }
            }
        }
    }

    if !script.commands.is_empty() {
        script
            .commands
            .insert(0, crate::git::Command::SwitchMark(base_mark));
    }
    if script.commands.is_empty() && script.dependents.is_empty() {
        None
    } else {
        Some(script)
    }
}
