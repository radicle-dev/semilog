#![feature(map_first_last)]

use std::collections::BTreeMap;

use semilattice::{GuardedPair, Map, Max, Redactable, SemiLattice, Set};

pub mod detailed;

/// An actor ID. Probably a public key.
pub type ActorID = String;

/// A Message ID. An actor ID paired with a supposedly unique number. The actor
/// is responsible for choosing a unique number.
pub type MessageID = (ActorID, u64);

pub type Reaction = String;
pub type Tag = String;

#[derive(Clone, Default, Debug, PartialEq, SemiLattice, minicbor::Encode, minicbor::Decode)]
pub struct Owned {
    #[n(0)]
    titles: GuardedPair<Max<u64>, Set<String>>,
    #[n(1)]
    reply_to: Set<MessageID>,
    #[n(2)]
    content: Map<u64, Redactable<String>>,
}

#[derive(Clone, Default, Debug, PartialEq, SemiLattice, minicbor::Encode, minicbor::Decode)]
pub struct Shared {
    #[n(0)]
    tags: Map<Tag, Max<u64>>,
    #[n(1)]
    reactions: Map<Tag, Max<u64>>,
}

#[derive(Clone, Default, Debug, PartialEq, SemiLattice, minicbor::Encode, minicbor::Decode)]
pub struct Slice {
    #[n(0)]
    owned: Map<u64, Owned>,
    #[n(1)]
    shared: Map<MessageID, Shared>,
}

#[derive(Clone, Default, Debug, PartialEq, SemiLattice, minicbor::Encode, minicbor::Decode)]
pub struct Root {
    #[n(0)]
    pub inner: Map<ActorID, Slice>,
}

#[derive(Debug)]
pub struct Actor<'a> {
    pub id: ActorID,
    pub device_id: u64,
    pub slice: &'a mut Slice,
}

impl Actor<'_> {
    pub fn new(slice: &mut Slice, id: ActorID, device_id: u64) -> Actor {
        Actor {
            id,
            device_id,
            slice,
        }
    }

    pub fn new_thread(
        &mut self,
        title: String,
        message: String,
        tags: impl IntoIterator<Item = String>,
    ) -> MessageID {
        let id = (u64::try_from(self.slice.owned.len()).unwrap() << 16) + self.device_id;

        self.slice.owned.entry_mut(id).join_assign(Owned {
            titles: GuardedPair {
                guard: Max(0),
                value: Set::singleton(title),
            },
            reply_to: Set::default(),
            content: Map::singleton(0, Redactable::Data(message)),
        });

        self.slice
            .shared
            .entry_mut((self.id.clone(), id))
            .tags
            .join_assign(
                tags.into_iter()
                    .map(|x| (x, Max(1)))
                    .collect::<BTreeMap<_, _>>()
                    .into(),
            );

        (self.id.clone(), id)
    }

    pub fn reply(&mut self, parent: MessageID, message: String) -> MessageID {
        let id = (u64::try_from(self.slice.owned.len()).unwrap() << 16) + self.device_id;

        self.slice.owned.entry_mut(id).join_assign(Owned {
            titles: Default::default(),
            reply_to: Set::singleton(parent),
            content: Map::singleton(0, Redactable::Data(message)),
        });

        (self.id.clone(), id)
    }

    pub fn edit(&mut self, id: u64, message: String) -> u64 {
        let content = &mut self.slice.owned.entry_mut(id).content;

        // One greater than the latest version we have observed.
        let version: u64 = (content
            .last_key_value()
            .map(|x| (x.0 >> 16) + 1)
            .unwrap_or(0)
            << 16)
            + self.device_id;

        content
            .entry_mut(version)
            .join_assign(Redactable::Data(message));

        version
    }

    /// Fails if you attempt to redact someone else' message.
    pub fn redact(&mut self, id: u64, version: u64) {
        self.slice
            .owned
            .entry_mut(id)
            .content
            .entry_mut(version)
            .join_assign(Redactable::Redacted);
    }

    pub fn react(&mut self, id: MessageID, reaction: Reaction, vote: bool) {
        let stored_vote = self
            .slice
            .shared
            .entry_mut(id)
            .reactions
            .entry_mut(reaction);

        if stored_vote.0 % 2 != vote as u64 {
            stored_vote.0 += 1;
        }
    }

    pub fn adjust_tags(
        &mut self,
        id: MessageID,
        add: impl IntoIterator<Item = Reaction>,
        remove: impl IntoIterator<Item = Reaction>,
    ) {
        let tags = &mut self.slice.shared.entry_mut(id).tags;

        for tag in add {
            let vote = tags.entry_mut(tag);
            // 0 = neutral, 1 = positive, 2 = negative, 3 = invalid
            match vote.0 % 4 {
                0 => vote.0 += 1,
                1 => (),
                2 => vote.0 += 3,
                _ => vote.0 += 2,
            }
        }

        for tag in remove {
            let vote = tags.entry_mut(tag);
            match vote.0 % 4 {
                0 => vote.0 += 2,
                1 => vote.0 += 1,
                2 => (),
                _ => vote.0 += 3,
            }
        }
    }
}

impl Root {
    pub fn save_actor_slice_to_git(&self, repo: &git2::Repository, actor_name: &str) {
        let mut buffer = Vec::new();

        minicbor::encode(self.inner.entry(actor_name), &mut buffer)
            .expect("Failed to CBOR encode actor slice.");

        let threads_tree = repo
            .find_reference("refs/threads")
            .and_then(|r| r.peel_to_tree());

        let mut tree = repo
            .treebuilder(threads_tree.ok().as_ref())
            .expect("Failed to create tree.");

        tree.insert(
            &actor_name,
            repo.blob(&buffer).expect("Failed to record blob."),
            0o160000,
        )
        .expect("Failed to insert blob into tree.");

        let tree_oid = tree.write().expect("Failed to write tree.");

        repo.reference("refs/threads", tree_oid, true, "log msg")
            .expect("Failed to update reference");
    }

    // Can panic; but the panics are occur on their own threads as an
    // implementation detail of git2...
    pub fn coalate_slices_into_root_from_git(repo: &git2::Repository) -> Root {
        let mut root = Root::default();

        let threads_tree = repo
            .find_reference("refs/threads")
            .and_then(|r| r.peel_to_tree());

        // Import each writer's slice.
        if let Ok(ref tree) = threads_tree {
            tree.walk(git2::TreeWalkMode::PreOrder, |_, entry| {
                let actor = entry.name().expect("Invalid reference name").to_owned();
                root.inner.entry_mut(actor).join_assign(
                    minicbor::decode(
                        entry
                            .to_object(repo)
                            .expect("Failed to lookup blob")
                            .peel_to_blob()
                            .expect("Expected blob!")
                            .content(),
                    )
                    .expect("Invalid CBOR"),
                );
                git2::TreeWalkResult::Ok
            })
            .expect("Failed to walk tree.");
        }

        root
    }

    /// Panics if the cache reference does not exist, does not point to a blob,
    /// or the blob cannot be read or decoded.
    pub fn load_cache_from_git(repo: &git2::Repository) -> Root {
        Root {
            inner: minicbor::decode(
                repo.find_reference("refs/threads-materialized")
                    .map(|r| r.peel_to_blob().expect("Expected blob"))
                    .expect("Failed to lookup reference")
                    .content(),
            )
            .expect("Failed to decode"),
        }
    }

    pub fn save_cache_to_git(&self, repo: &git2::Repository) {
        let mut buffer = Vec::new();

        minicbor::encode(&self.inner, &mut buffer).expect("Failed to CBOR encode root.");

        repo.reference(
            "refs/threads-materialized",
            repo.blob(&buffer).expect("Failed to write blob"),
            true,
            "log msg",
        )
        .expect("Failed to update reference");
    }
}
