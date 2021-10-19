use std::collections::BTreeMap;

use semilattice::{GuardedPair, Map, Max, Redactable, SemiLattice, Set};

use crate::{ActorID, MessageID, Owned, Reaction, Root, Shared, Tag, Vote};

#[derive(Default, Debug, Clone, SemiLattice, PartialEq, minicbor::Encode, minicbor::Decode)]
struct Thread {
    #[n(0)]
    titles: GuardedPair<Max<u64>, Set<String>>,
    #[n(1)]
    tags: Map<Tag, Vote<4>>,
}

#[derive(Default, Debug, Clone, SemiLattice, PartialEq, minicbor::Encode, minicbor::Decode)]
struct Comment {
    #[n(0)]
    reply_to: Set<MessageID>,
    #[n(1)]
    content: Map<u64, Redactable<String>>,
    #[n(2)]
    reactions: Map<Reaction, Vote<2>>,
    #[n(3)]
    backrefs: Set<MessageID>,
}

#[derive(Default, Debug, Clone, SemiLattice, PartialEq, minicbor::Encode, minicbor::Decode)]
pub struct Detailed {
    #[n(0)]
    threads: Map<ActorID, Map<u64, Thread>>,
    #[n(1)]
    messages: Map<ActorID, Map<u64, Comment>>,
}

impl SemiLattice<Root> for Detailed {
    fn join(mut self, other: Root) -> Self {
        for (actor, slice) in other.inner {
            let threads = self.threads.entry(actor);

            for (
                id,
                Owned {
                    titles,
                    reply_to,
                    content,
                },
            ) in slice.owned.inner
            {
                if titles.value.len() > 0 {
                    threads.entry(id).titles.join_assign(titles);
                }
                for br in &*reply_to {
                    self.messages.entry(br.0).entry(br.1).backrefs.insert((actor, id));
                }
                self.messages.entry(actor).entry(id).join_assign(Comment {
                    reply_to: reply_to,
                    content: content,
                    reactions: Map::default(),
                    backrefs: Set::default(),
                });
            }

            for ((aid, id), Shared { tags, reactions }) in slice.shared.inner {
                self.messages.entry(aid).entry(id).reactions.join_assign(
                    reactions
                        .inner
                        .into_iter()
                        .map(|(r, v)| (r, Vote(Map::singleton(actor, v))))
                        .collect::<BTreeMap<_, _>>()
                        .into(),
                );

                if tags.len() > 0 {
                    self.threads.entry(aid).entry(id).tags.join_assign(
                        tags
                            .inner
                            .into_iter()
                            .map(|(r, v)| (r, Vote(Map::singleton(actor, v))))
                            .collect::<BTreeMap<_, _>>()
                            .into(),
                    );
                }
            }
        }

        self
    }
}

impl Detailed {
    pub fn display(&self) {
        // An awful example UI.

        for (aid, thread) in &self.threads.inner {
            for (id, Thread { titles, tags }) in &thread.inner {
                println!("Author: {:?} [{}]", aid, id);
                for title in &titles.value.inner {
                    println!("Title: {}", title);
                }

                let mut tag_votes = BTreeMap::new();
                for (tag, votes) in &tags.inner {
                    let va = votes.aggregate();
                    *tag_votes.entry(tag).or_insert(0) += va[1] as i64 - va[2] as i64;
                }

                print!("Tags: ");
                for (tag, score) in tag_votes.into_iter().filter(|(_, x)| *x > 0) {
                    print!("{} ({}), ", tag, score);
                }
                println!();
                println!();

                let mut stack = vec![(0, (*aid, *id))];

                while let Some((depth, (aid, id))) = stack.pop() {
                    let message = self.messages.inner.get(&aid).expect("Expected aid").get(&id).expect("Expected id.");

                    stack.extend(message.backrefs.inner.clone().into_iter().map(|x| (depth + 1, x)));

                    println!("Depth: {}", depth);
                    println!("Author: {:?} [{}]", aid, id);
                    for (_, content) in &message.content.inner {
                        println!("Body: {:?}", content);
                    }
                    println!();
                }
            }
        }
    }
}