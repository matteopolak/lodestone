use std::collections::HashMap;

use super::{NodeId,selection::Selection,spline::{Spline,SplineValue}};

#[derive(Clone,Debug,Default)]
pub(super) struct Tables {
    pub selections: Vec<Selection>,
    pub splines: Vec<Spline>,
}

#[derive(Debug,Default)]
pub(super) struct TableBuilder {
    pub tables: Tables,
    selections: HashMap<Vec<u64>,usize>,
    splines: HashMap<Vec<u64>,usize>,
}

impl TableBuilder {
    pub fn selection(&mut self,value:Selection)->usize {
        let mut key=vec![value.input.0 as u64];
        key.extend(value.thresholds.iter().map(|v|u64::from(v.to_bits())));
        key.push(u64::MAX);
        key.extend(value.functions.iter().map(|id|id.0 as u64));
        if let Some(&id)=self.selections.get(&key) {return id;}
        let id=self.tables.selections.len();
        self.tables.selections.push(value);self.selections.insert(key,id);id
    }

    pub fn spline(&mut self,value:Spline)->usize {
        fn value_key(value:SplineValue)->[u64;2] {
            match value {SplineValue::Constant(v)=>[0,u64::from(v.to_bits())],SplineValue::Curve(id)=>[1,id as u64]}
        }
        let mut key=vec![value.coordinates.len() as u64];
        key.extend(value.coordinates.iter().map(|id|id.0 as u64));
        key.extend(value_key(value.root));
        for curve in &value.curves {
            key.extend([curve.coordinate as u64,curve.points.len() as u64]);
            for point in &curve.points {
                key.extend([u64::from(point.location.to_bits()),u64::from(point.derivative.to_bits())]);
                key.extend(value_key(point.value));
            }
        }
        if let Some(&id)=self.splines.get(&key) {return id;}
        let id=self.tables.splines.len();
        self.tables.splines.push(value);self.splines.insert(key,id);id
    }
}

impl Tables {
    pub fn selection_children(&self,id:usize)->Vec<NodeId> {
        let table=&self.selections[id];
        std::iter::once(table.input).chain(table.functions.iter().copied()).collect()
    }
}
