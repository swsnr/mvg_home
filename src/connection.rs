// Copyright Sebastian Wiesner <sebastian@swsnr.de>
//
// This Source Code Form is subject to the terms of the Mozilla Public
// License, v. 2.0. If a copy of the MPL was not distributed with this
// file, You can obtain one at http://mozilla.org/MPL/2.0/.

use serde::{Deserialize, Serialize};
use time::{Duration, OffsetDateTime};

use crate::mvg::Connection;

#[derive(Clone, PartialEq, Eq, Serialize, Deserialize, Debug)]
pub struct CompleteConnection {
    pub connection: Connection,
    pub walk_to_start: Duration,
}

impl CompleteConnection {
    pub fn start_to_walk(&self) -> OffsetDateTime {
        self.connection.departure - self.walk_to_start
    }
}

pub trait ConnectionExt {
    fn with_walk_to_start(self, walk_to_start: Duration) -> CompleteConnection;
}

impl ConnectionExt for Connection {
    fn with_walk_to_start(self, walk_to_start: Duration) -> CompleteConnection {
        CompleteConnection {
            connection: self,
            walk_to_start,
        }
    }
}
