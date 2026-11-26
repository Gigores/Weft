use crate::{Block, Tag, Vec3, World, abi, host};

/// How an entity travels to a goal and which traversable cells it prefers.
/// Build the options from the current behavior, components and equipment.
pub struct Navigation {
    medium: u32,
    costs: Vec<abi::RouteCost>,
    open_cost: f32,
}

impl Navigation {
    pub fn walking() -> Self {
        Self { medium: abi::navigation::WALK, costs: Vec::new(), open_cost: 1.0 }
    }

    /// The entity must declare which liquids it swims in with `SWIMS_IN`.
    pub fn swimming() -> Self {
        Self { medium: abi::navigation::SWIM, ..Self::walking() }
    }

    /// Cost per unit of travel through a block. One is neutral. Smaller positive weights
    /// favor a block and larger weights favor alternatives. The last matching rule wins.
    pub fn block_cost(mut self, block: &Block, multiplier: f32) -> Self {
        self.costs
            .push(abi::RouteCost { selector: abi::navigation::BLOCK, id: block.id(), multiplier });
        self
    }

    /// Apply a cost to all blocks with the tag.
    pub fn tag_cost(mut self, tag: &Tag, multiplier: f32) -> Self {
        self.costs
            .push(abi::RouteCost { selector: abi::navigation::TAG, id: tag.id(), multiplier });
        self
    }

    /// Multiply travel cost in open space. A weight above one prefers cells with a side wall.
    pub fn open_cost(mut self, multiplier: f32) -> Self {
        self.open_cost = multiplier;
        self
    }
}

impl World {
    /// Set or update a navigation request built with a movement and cost profile.
    /// A nonpositive speed clears the request.
    pub fn navigate_with(&self, id: u64, target: impl Into<Vec3>, speed: f32, profile: &Navigation) -> bool {
        let options = abi::RouteOptions { medium: profile.medium, costs: abi::Slice::new(&profile.costs), open_cost: profile.open_cost };
        unsafe { (host().navigate_with)(self.raw(), id, &target.into(), speed, &options) != 0 }
    }
}
