use super::*;

use crate::{check::nodes_clean_sig, ImplPrimitive::*, Node::*, Primitive::*};

pub(crate) const DEBUG: bool = false;

macro_rules! dbgln {
    ($($arg:tt)*) => {
        if DEBUG {
            println!($($arg)*); // Allow println
        }
    }
}
use dbgln;

impl Node {
    pub(super) fn optimize(&mut self) -> bool {
        let mut optimized = false;
        match self {
            Run(nodes) => {
                for node in nodes.make_mut() {
                    optimized |= node.optimize();
                }
                while OPTIMIZATIONS.iter().any(|op| {
                    if !op.match_and_replace(nodes) {
                        return false;
                    }
                    dbgln!("applied optimization {op:?}");
                    true
                }) {
                    optimized = true;
                }
                if nodes.len() == 1 {
                    *self = take(nodes).remove(0);
                }
            }
            Mod(_, args, _) | ImplMod(_, args, _) => {
                for arg in args.make_mut() {
                    optimized |= arg.node.optimize();
                }
            }
            Node::Array { inner, .. } => optimized |= Arc::make_mut(inner).optimize(),
            CustomInverse(cust, _) => {
                let cust = Arc::make_mut(cust);
                if let Ok(normal) = cust.normal.as_mut() {
                    optimized |= normal.node.optimize();
                }
                if let Some(un) = cust.un.as_mut() {
                    optimized |= un.node.optimize();
                }
                if let Some(anti) = cust.anti.as_mut() {
                    optimized |= anti.node.optimize();
                }
                if let Some((before, after)) = cust.under.as_mut() {
                    optimized |= before.node.optimize();
                    optimized |= after.node.optimize();
                }
            }
            _ => {}
        }
        optimized
    }
}

static OPTIMIZATIONS: &[&dyn Optimization] = &[
    &((Reverse, First), Last),
    &((Reverse, Last), First),
    &((Rise, First), FirstMinIndex),
    &((Fall, Last), LastMinIndex),
    &((Fall, First), FirstMaxIndex),
    &((Rise, Last), LastMaxIndex),
    &((Where, First), FirstWhere),
    &((Where, Last), LastWhere),
    &((Where, Len), LenWhere),
    &((Range, MemberOf), MemberOfRange),
    &((Range, 1, Rerank, MemberOf), MultidimMemberOfRange),
    &((Range, DeshapeSub(2), MemberOf), MultidimMemberOfRange),
    &((UnSort, Or(First, Last)), RandomRow),
    &((Deduplicate, Len), CountUnique),
    &((Dup, Rise, Select), Sort),
    &((Dup, Fall, Select), SortDown),
    &((Sort, Reverse), SortDown),
    &((SortDown, Reverse), Sort),
    &((Pop, Rand), ReplaceRand),
    &((Pop, Pop, Rand), ReplaceRand2),
    &((1, Flip, Div, Pow), Root),
    &((-1, Pow), (1, Flip, Div)),
    &((2, Pow), (Dup, Mul)),
    &ByToDup,
    &InlineCustomInverse,
    &TransposeOpt,
    &ReduceTableOpt,
    &ReduceDepthOpt,
    &AdjacentOpt,
    &AstarOpt,
    &SplitByOpt,
    &AllSameOpt,
    &RepeatRandOpt,
    &PopConst,
    &TraceOpt,
    &ValidateTypeOpt,
];

opt!(
    InlineCustomInverse,
    [CustomInverse(cust, _)](cust.normal.is_ok()),
    cust.normal.clone().unwrap().node
);

opt!(PopConst, [Push(_), Prim(Pop, _)], []);

opt!(
    TransposeOpt,
    (
        [Prim(Transpose, span), Prim(Transpose, _)],
        ImplPrim(TransposeN(2), *span)
    ),
    (
        [ImplPrim(TransposeN(n), span), Prim(Transpose, _)],
        ImplPrim(TransposeN(n + 1), *span)
    ),
    (
        [ImplPrim(TransposeN(a), span), ImplPrim(TransposeN(b), _)],
        ImplPrim(TransposeN(a + b), *span)
    ),
);

opt!(
    ReduceTableOpt,
    [Mod(Table, table_args, span), Mod(Reduce, reduce_args, _)](
        table_args[0].sig == (2, 1) && reduce_args[0].sig == (2, 1)
    ),
    ImplMod(
        ReduceTable,
        (reduce_args.iter().cloned())
            .chain(table_args.iter().cloned())
            .collect(),
        *span
    )
);

opt!(
    ValidateTypeOpt,
    (
        [
            Prim(Dup, _),
            Prim(Type, _),
            Push(val),
            ImplPrim(MatchPattern, span)
        ],
        [Push(val.clone()), ImplPrim(ValidateType, *span)]
    ),
    (
        [Prim(Type, _), Push(val), ImplPrim(MatchPattern, span)],
        [Push(val.clone()), ImplPrim(ValidateTypeConsume, *span)]
    )
);

#[derive(Debug)]
struct AdjacentOpt;
impl Optimization for AdjacentOpt {
    fn match_and_replace(&self, nodes: &mut EcoVec<Node>) -> bool {
        match_and_replace(nodes, |nodes| {
            let (args, span) = match nodes {
                [Prim(Windows, _), Mod(Rows, args, span), ..] => (args, span),
                [Mod(Stencil, st_args, _), Mod(Rows, args, span), ..]
                    if st_args.len() == 1 && matches!(st_args[0].node, Prim(Identity, _)) =>
                {
                    (args, span)
                }
                _ => return None,
            };
            let [f] = args.as_slice() else {
                return None;
            };
            match f.node.as_slice() {
                [Mod(Reduce, reduce_args, span)] if reduce_args[0].sig == (2, 1) => {
                    Some((2, ImplMod(Adjacent, reduce_args.clone(), *span)))
                }
                _ if args[0].sig == (1, 1) => Some((2, ImplMod(RowsWindows, args.clone(), *span))),
                _ => None,
            }
        })
    }
}

#[derive(Debug)]
struct ReduceDepthOpt;
impl Optimization for ReduceDepthOpt {
    fn match_and_replace(&self, nodes: &mut EcoVec<Node>) -> bool {
        match_and_replace(nodes, |nodes| {
            let [Mod(Rows, args, _), ..] = nodes else {
                return None;
            };
            let [f] = args.as_slice() else {
                return None;
            };
            match f.node.as_slice() {
                [Mod(Reduce, redu_args, span)] => {
                    Some((1, ImplMod(ReduceDepth(1), redu_args.clone(), *span)))
                }
                [ImplMod(ReduceDepth(depth), redu_args, span)] => {
                    Some((1, ImplMod(ReduceDepth(depth + 1), redu_args.clone(), *span)))
                }
                _ => None,
            }
        })
    }
}

opt!(
    AstarOpt,
    (
        [Mod(Astar, args, span), Prim(First, _)],
        ImplMod(AstarFirst, args.clone(), *span)
    ),
    (
        [Mod(Astar, args, span), Prim(Pop, _)],
        ImplMod(AstarPop, args.clone(), *span)
    ),
);

#[derive(Debug)]
struct AllSameOpt;
impl Optimization for AllSameOpt {
    fn match_and_replace(&self, nodes: &mut EcoVec<Node>) -> bool {
        match_and_replace(nodes, |nodes| match nodes {
            [Prim(Dup, span), Push(val), Prim(Rotate, _), Prim(Match, _), ..]
                if *val == 1 || *val == -1 =>
            {
                Some((4, ImplPrim(AllSame, *span)))
            }
            [Mod(By | On, args, span), Prim(Match, _), ..] => {
                let [f] = args.as_slice() else {
                    return None;
                };
                match f.node.as_slice() {
                    [Push(val), Prim(Rotate, _)] if *val == 1 || *val == -1 => {}
                    _ => return None,
                }
                Some((2, ImplPrim(AllSame, *span)))
            }
            [ImplPrim(CountUnique, span), Push(val), Prim(Le, _), ..] if *val == 1 => {
                Some((3, ImplPrim(AllSame, *span)))
            }
            _ => None,
        })
    }
}

#[derive(Debug)]
struct SplitByOpt;
impl Optimization for SplitByOpt {
    fn match_and_replace(&self, nodes: &mut EcoVec<Node>) -> bool {
        fn par_f(node: &Node) -> Option<SigNode> {
            let Mod(Partition, args, _) = node else {
                return None;
            };
            let [f] = args.as_slice() else {
                return None;
            };
            if f.sig.args != 1 {
                return None;
            }
            Some(f.clone())
        }
        match_and_replace(nodes, |nodes| match nodes {
            [Mod(By, args, span), last, ..]
                if matches!(args.as_slice(), [f]
                            if matches!(f.node, Prim(Ne, _))) =>
            {
                let f = par_f(last)?;
                Some((2, ImplMod(SplitByScalar, eco_vec![f], *span)))
            }
            [Mod(By, args, span), Prim(Not, _), last, ..]
                if matches!(args.as_slice(), [f]
                            if matches!(f.node, Prim(Mask, _))) =>
            {
                let f = par_f(last)?;
                Some((3, ImplMod(SplitBy, eco_vec![f], *span)))
            }
            [Prim(Dup, span), Push(delim), Prim(Ne, _), last, ..] => {
                let f = par_f(last)?;
                let new = Node::from_iter([
                    Push(delim.clone()),
                    ImplMod(SplitByScalar, eco_vec![f], *span),
                ]);
                Some((4, new))
            }
            [Prim(Dup, span), Push(delim), Prim(Mask, _), Prim(Not, _), last, ..] => {
                let f = par_f(last)?;
                let new =
                    Node::from_iter([Push(delim.clone()), ImplMod(SplitBy, eco_vec![f], *span)]);
                Some((5, new))
            }
            _ => None,
        })
    }
}

#[derive(Debug)]
struct RepeatRandOpt;
impl Optimization for RepeatRandOpt {
    fn match_and_replace(&self, nodes: &mut EcoVec<Node>) -> bool {
        match_and_replace(nodes, |mut nodes| {
            // Extract potential repetition count before array
            let n = if let [Node::Push(n), rest @ ..] = nodes {
                nodes = rest;
                Some(n)
            } else {
                None
            };
            let had_leading_n = n.is_some();
            // Extract array
            let [Node::Array { inner, .. }, ..] = nodes else {
                return None;
            };
            let mut inner = inner.as_slice();
            // Extract repetition count inside array if it wasn't extracted before
            let n = if let Some(n) = n {
                n
            } else if let [Push(n), rest @ ..] = inner {
                inner = rest;
                n
            } else {
                return None;
            };
            if n.rank() != 0 {
                return None;
            }
            // Extract repeat
            let [Mod(Repeat, args, repeat_span) | ImplMod(RepeatWithInverse, args, repeat_span)] =
                inner
            else {
                return None;
            };
            let f = args.first()?;

            match f.node.as_slice() {
                [Prim(Rand, rand_span), Push(max), mul @ Prim(Mul, _), floor @ Prim(Floor, _)] => {
                    if max.rank() != 0 {
                        return None;
                    }
                    let new = Node::from_iter([
                        Push(n.clone()),
                        Prim(Range, *repeat_span),
                        Mod(
                            Rows,
                            eco_vec![SigNode::new((1, 1), ImplPrim(ReplaceRand, *rand_span))],
                            *repeat_span,
                        ),
                        Push(max.clone()),
                        mul.clone(),
                        floor.clone(),
                    ]);
                    Some((1 + had_leading_n as usize, new))
                }
                [Prim(Rand, rand_span)] => {
                    let new = Node::from_iter([
                        Push(n.clone()),
                        Prim(Range, *repeat_span),
                        Mod(
                            Rows,
                            eco_vec![SigNode::new((1, 1), ImplPrim(ReplaceRand, *rand_span))],
                            *repeat_span,
                        ),
                    ]);
                    Some((1 + had_leading_n as usize, new))
                }
                _ => None,
            }
        })
    }
}

opt!(
    TraceOpt,
    (
        [Prim(Trace, span), Prim(Trace, _)],
        ImplPrim(
            StackN {
                n: 2,
                inverse: false,
            },
            *span
        )
    ),
    (
        [ImplPrim(StackN { n, inverse: false }, span), Prim(Trace, _)],
        ImplPrim(
            StackN {
                n: n + 1,
                inverse: false,
            },
            *span
        )
    ),
);

#[derive(Debug)]
struct ByToDup;
impl Optimization for ByToDup {
    fn match_and_replace(&self, nodes: &mut EcoVec<Node>) -> bool {
        'outer: for i in 0..nodes.len() {
            let Mod(By, args, span) = &nodes[i] else {
                continue;
            };
            let [f] = args.as_slice() else {
                continue;
            };
            let mut back = 0;
            let mut dip = false;
            'back: {
                if f.sig != (1, 1) {
                    if f.sig == (2, 1) {
                        for j in (0..i).rev() {
                            let frag = &nodes[j..i];
                            if nodes_clean_sig(frag)
                                .is_some_and(|sig| sig == (0, 1) || sig == (1, 2))
                            {
                                // println!("frag: {frag:?}");
                                dip = !matches!(frag, [Prim(Dup, _)]);
                                back = i - j;
                                break 'back;
                            }
                        }
                        dip = true;
                    } else {
                        continue 'outer;
                    }
                }
            }
            let mut composed = Node::from_iter(nodes[i - back..i].iter().cloned());
            let mut dup = Prim(Dup, *span);
            if dip {
                dup = Mod(Dip, eco_vec![dup.sig_node().unwrap()], *span);
            }
            composed.push(dup);
            composed.push(f.node.clone());
            composed.extend(nodes[i + 1..].iter().cloned());
            // println!("composed: {composed:?}");
            if composed.optimize() {
                let n = nodes.len() - i;
                replace_nodes(nodes, i - back, n + back, composed);
                // println!("optimized: {nodes:?}");
                return true;
            }
        }
        false
    }
}

trait Optimization: Debug + Sync {
    fn match_and_replace(&self, nodes: &mut EcoVec<Node>) -> bool;
}

impl<A, B> Optimization for (A, B)
where
    A: OptPattern,
    B: OptReplace,
{
    fn match_and_replace(&self, nodes: &mut EcoVec<Node>) -> bool {
        match_and_replace(nodes, |nodes| {
            let (n, Some(span)) = self.0.match_nodes(nodes)? else {
                return None;
            };
            Some((n, self.1.replacement_node(span)))
        })
    }
}

trait OptPattern: Debug + Sync {
    fn match_nodes(&self, nodes: &[Node]) -> Option<(usize, Option<usize>)>;
}
impl OptPattern for Primitive {
    fn match_nodes(&self, nodes: &[Node]) -> Option<(usize, Option<usize>)> {
        match nodes {
            [Node::Prim(p, span), ..] if self == p => Some((1, Some(*span))),
            _ => None,
        }
    }
}
impl OptPattern for ImplPrimitive {
    fn match_nodes(&self, nodes: &[Node]) -> Option<(usize, Option<usize>)> {
        match nodes {
            [Node::ImplPrim(p, span), ..] if self == p => Some((1, Some(*span))),
            _ => None,
        }
    }
}
impl OptPattern for i32 {
    fn match_nodes(&self, nodes: &[Node]) -> Option<(usize, Option<usize>)> {
        match nodes {
            [Node::Push(n), ..] if n == self => Some((1, None)),
            _ => None,
        }
    }
}
#[derive(Debug)]
struct Or<A, B>(A, B);
impl<A, B> OptPattern for Or<A, B>
where
    A: OptPattern,
    B: OptPattern,
{
    fn match_nodes(&self, nodes: &[Node]) -> Option<(usize, Option<usize>)> {
        self.0
            .match_nodes(nodes)
            .or_else(|| self.1.match_nodes(nodes))
    }
}

trait OptReplace: Debug + Sync {
    fn replacement_node(&self, span: usize) -> Node;
}
impl OptReplace for Primitive {
    fn replacement_node(&self, span: usize) -> Node {
        Node::Prim(*self, span)
    }
}
impl OptReplace for ImplPrimitive {
    fn replacement_node(&self, span: usize) -> Node {
        Node::ImplPrim(*self, span)
    }
}
impl OptReplace for i32 {
    fn replacement_node(&self, _: usize) -> Node {
        Node::new_push(*self)
    }
}

fn replace_nodes(nodes: &mut EcoVec<Node>, i: usize, n: usize, new: Node) {
    // dbg!(&nodes, i, n, &new);
    let orig_len = nodes.len();
    debug_assert!(orig_len - n >= i);
    nodes.make_mut()[i..].rotate_left(n);
    nodes.truncate(orig_len - n);
    nodes.extend(new);
    let added = nodes.len() - (orig_len - n);
    nodes.make_mut()[i..].rotate_right(added);
}

fn match_and_replace(
    nodes: &mut EcoVec<Node>,
    f: impl Fn(&[Node]) -> Option<(usize, Node)>,
) -> bool {
    for i in 0..nodes.len() {
        if let Some((n, node)) = f(&nodes[i..]) {
            replace_nodes(nodes, i, n, node);
            return true;
        }
    }
    false
}

macro_rules! opt {
    ($name:ident, [$($pat:pat),*] $(($cond:expr))?, $new:expr) => {
        opt!($name, ([$($pat),*] $(($cond))?, $new));
    };
    ($name:ident, $(([$($pat:pat),*] $(($cond:expr))?, $new:expr)),* $(,)?) => {
        #[derive(Debug)]
        struct $name;
        impl Optimization for $name {
            fn match_and_replace(&self, nodes: &mut EcoVec<Node>) -> bool {
                for i in 0..nodes.len() {
                    match &nodes[i..] {
                        $(
                            [$($pat),*, ..] $(if $cond)? => {
                                const N: usize = 0 $(+ {stringify!($pat); 1})*;
                                let new = $new;
                                replace_nodes(nodes, i, N, new.into());
                                return true;
                            }
                        )*
                        _ => {}
                    }
                }
                false
            }
        }
    };
}
use fmt::Debug;
use opt;

macro_rules! opt_pattern {
    ($($T:ident),*) => {
        impl<$($T),*> OptPattern for ($($T,)*)
        where
            $($T: OptPattern,)*
        {
            #[allow(unused, non_snake_case)]
            fn match_nodes(&self, mut nodes: &[Node]) -> Option<(usize, Option<usize>)> {
                let ($($T,)*) = self;
                let mut i = 0;
                let mut span = None;
                $(
                    let (n, sp) = $T.match_nodes(nodes)?;
                    i += n;
                    nodes = &nodes[n..];
                    if sp.is_some() {
                        span = sp;
                    }
                )*
                Some((i, span))
            }
        }
        #[allow(non_snake_case)]
        impl<$($T),*> OptReplace for ($($T,)*)
        where
            $($T: OptReplace,)*
        {
            fn replacement_node(&self, span: usize) -> Node {
                let ($($T,)*) = self;
                Node::from_iter([
                    $($T.replacement_node(span),)*
                ])
            }
        }
    }
}

opt_pattern!(A, B);
opt_pattern!(A, B, C);
opt_pattern!(A, B, C, D);
