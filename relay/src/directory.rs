//! The sessions copies list for matchmaking, and the search over them. It holds
//! no sockets, so what it decides can be read from its tally of why a listing
//! did not fit.

use crate::protocol::SESSION_SIZE;
use std::collections::BTreeMap;

#[derive(Clone)]
pub struct Listing {
    pub rtt: u16,
    pub session: [u8; SESSION_SIZE],
    pub open_public: u32,
    pub open_private: u32,
    pub filled_public: u32,
    pub filled_private: u32,
    pub attributes: Vec<(u32, u32)>,
}

impl Listing {
    fn value(&self, id: u32) -> Option<u32> {
        self.attributes.iter().find(|(key, _)| *key == id).map(|(_, value)| *value)
    }

    fn refused_by<'a>(&self, wants: &'a [Wanted]) -> Option<&'a Wanted> {
        wants.iter().find(|want| match self.value(want.id) {
            None => true,
            Some(value) if want.at_least => value < want.value,
            Some(value) => value != want.value,
        })
    }
}

#[derive(Clone, Copy)]
pub struct Wanted {
    pub id: u32,
    pub value: u32,
    pub at_least: bool,
}

/// Why listings did not fit one search, for the log and the status page.
#[derive(Default)]
pub struct Tally {
    pub listed: usize,
    pub full: usize,
    pub refused_by: BTreeMap<u32, usize>, // by attribute id
}

#[derive(Default)]
pub struct Directory {
    listings: BTreeMap<u64, Listing>, // by the copy's id
    sequences: BTreeMap<u64, u32>,    // the newest each copy sent
}

impl Directory {
    /// One listing a copy, the newest. False for a late packet.
    pub fn put(&mut self, host: u64, sequence: u32, listing: Listing) -> bool {
        if !self.newer(host, sequence) {
            return false;
        }
        self.listings.insert(host, listing);
        true
    }

    /// True when a listing went.
    pub fn withdraw(&mut self, host: u64, sequence: u32) -> bool {
        self.newer(host, sequence) && self.listings.remove(&host).is_some()
    }

    pub fn forget(&mut self, host: u64) {
        self.listings.remove(&host);
        self.sequences.remove(&host);
    }

    /// The listings that fit, nearest to the relay first: every packet of a
    /// match goes through it, so that is the order of their lag.
    pub fn search(
        &self,
        asker: u64,
        room: u32,
        wants: &[Wanted],
        most: usize,
        tally: &mut Tally,
    ) -> Vec<(u64, &Listing)> {
        let mut found = Vec::new();
        for (&host, listing) in &self.listings {
            if host == asker {
                continue;
            }
            tally.listed += 1;
            if listing.open_public < room {
                tally.full += 1;
            } else if let Some(refused) = listing.refused_by(wants) {
                *tally.refused_by.entry(refused.id).or_default() += 1;
            } else {
                found.push((host, listing));
            }
        }
        found.sort_by_key(|(_, listing)| listing.rtt);
        found.truncate(most);
        found
    }

    pub fn listings(&self) -> &BTreeMap<u64, Listing> {
        &self.listings
    }

    pub fn holds(&self, host: u64) -> bool {
        self.listings.contains_key(&host)
    }

    fn newer(&mut self, host: u64, sequence: u32) -> bool {
        match self.sequences.get_mut(&host) {
            Some(newest) if sequence <= *newest => false,
            Some(newest) => {
                *newest = sequence;
                true
            }
            None => {
                self.sequences.insert(host, sequence);
                true
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn listing(rtt: u16, open: u32, attributes: &[(u32, u32)]) -> Listing {
        Listing {
            rtt,
            session: [0; SESSION_SIZE],
            open_public: open,
            open_private: 0,
            filled_public: 0,
            filled_private: 0,
            attributes: attributes.to_vec(),
        }
    }

    fn hosts(found: Vec<(u64, &Listing)>) -> Vec<u64> {
        found.into_iter().map(|(host, _)| host).collect()
    }

    #[test]
    fn finds_what_fits_and_says_why_not() {
        let mut directory = Directory::default();
        directory.put(1, 1, listing(5, 8, &[(7, 1), (9, 8)]));
        directory.put(2, 1, listing(5, 8, &[(7, 2), (9, 8)]));
        directory.put(3, 1, listing(5, 8, &[(7, 1), (9, 1)]));
        directory.put(4, 1, listing(5, 0, &[(7, 1), (9, 8)]));
        directory.put(5, 1, listing(5, 8, &[(9, 8)]));
        let wants = [
            Wanted { id: 7, value: 1, at_least: false },
            Wanted { id: 9, value: 2, at_least: true },
        ];
        let mut tally = Tally::default();
        assert_eq!(hosts(directory.search(0, 1, &wants, 50, &mut tally)), [1]);
        assert_eq!(tally.listed, 5);
        assert_eq!(tally.full, 1);
        assert_eq!(tally.refused_by[&7], 2);
        assert_eq!(tally.refused_by[&9], 1);
    }

    #[test]
    fn never_finds_the_asker() {
        let mut directory = Directory::default();
        directory.put(1, 1, listing(5, 8, &[]));
        assert!(directory.search(1, 0, &[], 50, &mut Tally::default()).is_empty());
    }

    #[test]
    fn nearest_first_and_no_more_than_asked() {
        let mut directory = Directory::default();
        directory.put(1, 1, listing(30, 8, &[]));
        directory.put(2, 1, listing(10, 8, &[]));
        directory.put(3, 1, listing(20, 8, &[]));
        assert_eq!(hosts(directory.search(0, 0, &[], 2, &mut Tally::default())), [2, 3]);
    }

    #[test]
    fn a_late_packet_undoes_nothing() {
        let mut directory = Directory::default();
        assert!(directory.put(1, 5, listing(1, 8, &[])));
        assert!(!directory.put(1, 4, listing(2, 8, &[])));
        assert!(!directory.withdraw(1, 5));
        assert_eq!(directory.listings()[&1].rtt, 1);
        assert!(directory.withdraw(1, 6));
        assert!(!directory.put(1, 6, listing(3, 8, &[])));
        assert!(directory.put(1, 7, listing(3, 8, &[])));
    }

    #[test]
    fn a_copy_forgotten_starts_again() {
        let mut directory = Directory::default();
        directory.put(1, 9, listing(1, 8, &[]));
        directory.forget(1);
        assert!(!directory.holds(1));
        assert!(directory.put(1, 1, listing(1, 8, &[])));
    }
}
