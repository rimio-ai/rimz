//! Pure tmux layout trees: legacy and JSON reads, legacy writes, and stack lifting.

use serde::Deserialize;

use crate::mux::SplitDirection;

#[derive(Clone, Debug, PartialEq, Eq)]
pub(super) struct Layout {
    pub(super) root: Cell,
    floating: Vec<String>,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub(super) struct Cell {
    pub(super) width: u64,
    height: u64,
    pub(super) x: i64,
    y: i64,
    content: Content,
}

#[derive(Clone, Debug, PartialEq, Eq)]
enum Content {
    Pane(String),
    Columns(Vec<Cell>),
    Rows(Vec<Cell>),
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, thiserror::Error)]
pub(super) enum Refusal {
    #[error("window has floating panes")]
    Floating,
    #[error("layout leaf order differs from pane-list order")]
    Order,
    #[error("stack members are not consecutive leaves of one row container")]
    Members,
    #[error("stack has too few rows for a downward split")]
    Height,
    #[error("stack has too few columns for the staging split")]
    Width,
    #[error("new pane is no longer directly after the last stack member")]
    Newcomer,
    #[error("a pane outside the split moved or resized during it")]
    Moved,
}

impl Layout {
    pub(super) fn parse(input: &str) -> Option<Self> {
        if input.starts_with('{') {
            let json: JsonLayout = serde_json::from_str(input).ok()?;
            if json.version != 2 {
                return None;
            }
            let mut floating = Vec::new();
            let root = json.root.into_cell(&mut floating)?;
            return Some(Self { root, floating });
        }
        let (_, body) = input.split_once(',')?;
        let mut reader = LegacyReader {
            input: body.as_bytes(),
            pos: 0,
        };
        let root = reader.cell(0)?;
        let mut floating = Vec::new();
        // tmux 3.7 duplicates floats in the root and a trailing suffix; only
        // the suffix marks them. A zoomed layout has no floating suffix.
        if reader.peek() == Some(b'<') {
            reader.pos += 1;
            loop {
                let cell = reader.cell(0)?;
                let mut leaves = Vec::new();
                cell.leaves(&mut leaves);
                floating.extend(
                    leaves
                        .into_iter()
                        .filter_map(Cell::pane_id)
                        .map(str::to_owned),
                );
                match reader.next()? {
                    b',' => {}
                    b'>' => break,
                    _ => return None,
                }
            }
        }
        (reader.pos == reader.input.len()).then_some(Self { root, floating })
    }

    pub(super) fn leaves(&self) -> Vec<&Cell> {
        let mut leaves = Vec::new();
        self.root.leaves(&mut leaves);
        leaves.retain(|cell| {
            !self
                .floating
                .iter()
                .any(|id| cell.pane_id() == Some(id.as_str()))
        });
        leaves
    }

    pub(super) fn legacy(&self) -> String {
        let mut body = String::new();
        self.root.write(&mut body);
        with_checksum(&body)
    }

    pub(super) fn check_stack(
        &self,
        order: &[String],
        members: &[String],
        open: Option<&str>,
        direction: SplitDirection,
    ) -> Result<(), Refusal> {
        self.check_order(order)?;
        let run = self.root.find_run(members).ok_or(Refusal::Members)?;
        run.extents(open, direction)?;
        Ok(())
    }

    fn check_order(&self, order: &[String]) -> Result<(), Refusal> {
        if !self.floating.is_empty() {
            return Err(Refusal::Floating);
        }
        if !self
            .leaves()
            .iter()
            .filter_map(|cell| cell.pane_id())
            .eq(order.iter().map(String::as_str))
        {
            return Err(Refusal::Order);
        }
        Ok(())
    }

    pub(super) fn beside_stack(
        &self,
        order: &[String],
        members: &[String],
        open: Option<&str>,
        newcomer: &str,
        direction: SplitDirection,
    ) -> Result<Self, Refusal> {
        self.check_stack(order, members, open, direction)?;
        let mut result = self.clone();
        if !result.root.lift(members, open, newcomer, direction)? {
            return Err(Refusal::Members);
        }
        Ok(result)
    }

    pub(super) fn check_split_order(
        &self,
        order: &[String],
        before: &[String],
        last: &str,
        newcomer: &str,
    ) -> Result<(), Refusal> {
        self.check_order(order)?;
        let index = before
            .iter()
            .position(|id| id == last)
            .ok_or(Refusal::Newcomer)?;
        let mut expected = before.to_vec();
        expected.insert(index + 1, newcomer.to_owned());
        if order != expected {
            return Err(Refusal::Newcomer);
        }
        Ok(())
    }

    pub(super) fn check_split_geometry(
        &self,
        before: &Self,
        last: &str,
        newcomer: &str,
    ) -> Result<(), Refusal> {
        // A window shrink can move only the open last member, and a legacy
        // select-layout would resize the window back to the read's size.
        if (self.root.width, self.root.height) != (before.root.width, before.root.height)
            || !self
                .leaves()
                .into_iter()
                .filter(|cell| cell.pane_id() != Some(last) && cell.pane_id() != Some(newcomer))
                .eq(before
                    .leaves()
                    .into_iter()
                    .filter(|cell| cell.pane_id() != Some(last)))
        {
            return Err(Refusal::Moved);
        }
        Ok(())
    }
}

impl Cell {
    pub(super) fn pane_id(&self) -> Option<&str> {
        match &self.content {
            Content::Pane(id) => Some(id),
            Content::Columns(_) | Content::Rows(_) => None,
        }
    }

    fn leaves<'a>(&'a self, leaves: &mut Vec<&'a Self>) {
        match &self.content {
            Content::Pane(_) => leaves.push(self),
            Content::Columns(children) | Content::Rows(children) => {
                for child in children {
                    child.leaves(leaves);
                }
            }
        }
    }

    fn write(&self, body: &mut String) {
        body.push_str(&format!(
            "{}x{},{},{}",
            self.width, self.height, self.x, self.y
        ));
        let (children, start, end) = match &self.content {
            Content::Pane(id) => {
                body.push(',');
                body.push_str(id.trim_start_matches('%'));
                return;
            }
            Content::Columns(children) => (children, '{', '}'),
            Content::Rows(children) => (children, '[', ']'),
        };
        body.push(start);
        for (index, child) in children.iter().enumerate() {
            if index > 0 {
                body.push(',');
            }
            child.write(body);
        }
        body.push(end);
    }

    fn run(rows: &[Self]) -> Option<Self> {
        let first = rows.first()?;
        let last = rows.last()?;
        Some(Self {
            width: first.width,
            height: u64::try_from(last.y.checked_sub(first.y)?)
                .ok()?
                .checked_add(last.height)?,
            x: first.x,
            y: first.y,
            content: Content::Rows(rows.to_vec()),
        })
    }

    fn find_run(&self, members: &[String]) -> Option<Self> {
        match &self.content {
            Content::Pane(_) => None,
            Content::Columns(children) | Content::Rows(children) => {
                if matches!(self.content, Content::Rows(_))
                    && let Some(range) = member_range(children, members)
                {
                    return Self::run(&children[range]);
                }
                children.iter().find_map(|child| child.find_run(members))
            }
        }
    }

    fn open_or_tallest(rows: &[Self], open: Option<&str>) -> Option<usize> {
        rows.iter()
            .enumerate()
            .filter(|(_, cell)| open.is_none_or(|id| cell.pane_id() == Some(id)))
            .max_by_key(|(_, cell)| cell.height)
            .map(|(index, _)| index)
    }

    fn extents(
        &self,
        open: Option<&str>,
        direction: SplitDirection,
    ) -> Result<(u64, u64), Refusal> {
        let Content::Rows(rows) = &self.content else {
            return Err(Refusal::Members);
        };
        if self.width < 3 {
            return Err(Refusal::Width);
        }
        let extent = match direction {
            SplitDirection::Right => self.width,
            SplitDirection::Down => self.height,
        };
        let minimum = match direction {
            SplitDirection::Right => Refusal::Width,
            SplitDirection::Down => Refusal::Height,
        };
        let new = extent.checked_sub(1).ok_or(minimum)? / 2;
        if new == 0 {
            return Err(minimum);
        }
        let keep = extent - 1 - new;
        if direction == SplitDirection::Down {
            let expanded = Self::open_or_tallest(rows, open).ok_or(Refusal::Members)?;
            let collapsed = rows
                .iter()
                .enumerate()
                .filter(|(index, _)| *index != expanded)
                .map(|(_, cell)| cell.height)
                .max()
                .unwrap_or(0);
            if rows[expanded]
                .height
                .checked_sub(extent - keep)
                .is_none_or(|height| height <= collapsed)
            {
                return Err(Refusal::Height);
            }
        }
        Ok((keep, new))
    }

    fn split_run(
        mut self,
        open: Option<&str>,
        newcomer: &str,
        direction: SplitDirection,
    ) -> Result<Self, Refusal> {
        let (keep, new) = self.extents(open, direction)?;
        let mut neighbour = Self {
            width: self.width,
            height: self.height,
            x: self.x,
            y: self.y,
            content: Content::Pane(newcomer.to_owned()),
        };
        let mut run = self.clone();
        let Content::Rows(rows) = &mut run.content else {
            return Err(Refusal::Members);
        };
        match direction {
            SplitDirection::Right => {
                run.width = keep;
                for row in rows {
                    row.width = keep;
                }
                neighbour.width = new;
                neighbour.x += (keep + 1) as i64;
                self.content = Content::Columns(vec![run, neighbour]);
            }
            SplitDirection::Down => {
                run.height = keep;
                let expanded = Self::open_or_tallest(rows, open).ok_or(Refusal::Members)?;
                let mut y = self.y;
                for (index, row) in rows.iter_mut().enumerate() {
                    if index == expanded {
                        row.height -= self.height - keep;
                    }
                    row.y = y;
                    y += (row.height + 1) as i64;
                }
                neighbour.height = new;
                neighbour.y += (keep + 1) as i64;
                self.content = Content::Rows(vec![run, neighbour]);
            }
        }
        Ok(self)
    }

    fn lift(
        &mut self,
        members: &[String],
        open: Option<&str>,
        newcomer: &str,
        direction: SplitDirection,
    ) -> Result<bool, Refusal> {
        if let Content::Rows(rows) = &mut self.content
            && let Some(range) = member_range(rows, members)
        {
            let replacement = Self::run(&rows[range.clone()])
                .ok_or(Refusal::Members)?
                .split_run(open, newcomer, direction)?;
            if range.len() == rows.len() {
                *self = replacement;
            } else {
                rows.splice(range, [replacement]);
            }
            return Ok(true);
        }
        if let Content::Columns(children) | Content::Rows(children) = &mut self.content {
            for child in children {
                if child.lift(members, open, newcomer, direction)? {
                    return Ok(true);
                }
            }
        }
        Ok(false)
    }
}

fn member_range(rows: &[Cell], members: &[String]) -> Option<std::ops::Range<usize>> {
    if members.len() < 2 || members.len() > rows.len() {
        return None;
    }
    let start = rows.windows(members.len()).position(|cells| {
        cells
            .iter()
            .zip(members)
            .all(|(cell, id)| cell.pane_id() == Some(id.as_str()))
    })?;
    Some(start..start + members.len())
}

pub(super) fn with_checksum(body: &str) -> String {
    let checksum = body.bytes().fold(0_u16, |sum, byte| {
        sum.rotate_right(1).wrapping_add(u16::from(byte))
    });
    format!("{checksum:04x},{body}")
}

#[derive(Deserialize)]
struct JsonLayout {
    #[serde(rename = "V")]
    version: u8,
    #[serde(rename = "L")]
    root: JsonCell,
}

#[derive(Deserialize)]
struct JsonCell {
    t: char,
    w: u32,
    h: u32,
    x: i32,
    y: i32,
    #[serde(default)]
    c: Vec<Self>,
    #[serde(rename = "I")]
    id: Option<String>,
    z: Option<u32>,
}

impl JsonCell {
    fn into_cell(self, floating: &mut Vec<String>) -> Option<Cell> {
        let content = match self.t {
            'p' => {
                let id = self.id?;
                id.strip_prefix('%')?.parse::<u32>().ok()?;
                if self.z.is_some() {
                    floating.push(id.clone());
                }
                Content::Pane(id)
            }
            'h' | 'v' => {
                if self.c.is_empty() {
                    return None;
                }
                let children = self
                    .c
                    .into_iter()
                    .map(|cell| cell.into_cell(floating))
                    .collect::<Option<Vec<_>>>()?;
                if self.t == 'h' {
                    Content::Columns(children)
                } else {
                    Content::Rows(children)
                }
            }
            _ => return None,
        };
        Some(Cell {
            width: u64::from(self.w),
            height: u64::from(self.h),
            x: i64::from(self.x),
            y: i64::from(self.y),
            content,
        })
    }
}

struct LegacyReader<'a> {
    input: &'a [u8],
    pos: usize,
}

impl LegacyReader<'_> {
    fn cell(&mut self, depth: usize) -> Option<Cell> {
        if depth > 128 {
            return None;
        }
        let width = self.number()?;
        self.consume(b'x')?;
        let height = self.number()?;
        self.consume(b',')?;
        let x = self.offset()?;
        self.consume(b',')?;
        let y = self.offset()?;
        let content = match self.next()? {
            b',' => Content::Pane(format!("%{}", self.number()?)),
            start @ (b'{' | b'[') => {
                let end = if start == b'{' { b'}' } else { b']' };
                let mut children = Vec::new();
                loop {
                    children.push(self.cell(depth + 1)?);
                    match self.next()? {
                        b',' => {}
                        byte if byte == end => break,
                        _ => return None,
                    }
                }
                if start == b'{' {
                    Content::Columns(children)
                } else {
                    Content::Rows(children)
                }
            }
            _ => return None,
        };
        Some(Cell {
            width,
            height,
            x,
            y,
            content,
        })
    }

    fn number(&mut self) -> Option<u64> {
        let start = self.pos;
        while self.peek().is_some_and(|byte| byte.is_ascii_digit()) {
            self.pos += 1;
        }
        std::str::from_utf8(&self.input[start..self.pos])
            .ok()?
            .parse::<u32>()
            .ok()
            .map(u64::from)
    }

    fn offset(&mut self) -> Option<i64> {
        let negative = self.peek() == Some(b'-');
        if negative {
            self.pos += 1;
        }
        let value = i64::try_from(self.number()?).ok()?;
        Some(if negative { -value } else { value })
    }

    fn consume(&mut self, expected: u8) -> Option<()> {
        (self.next()? == expected).then_some(())
    }

    fn next(&mut self) -> Option<u8> {
        let byte = self.peek()?;
        self.pos += 1;
        Some(byte)
    }

    fn peek(&self) -> Option<u8> {
        self.input.get(self.pos).copied()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    // Legacy: tmux 3.5a, 3.7c, and a 3.8 control client. JSON: an ordinary 3.8 client.
    const LEGACY: &str = "7a93,160x60,0,0{80x60,0,0,0,79x60,81,0[79x30,81,0,1,79x29,81,31,2]}";
    const JSON: &str = r#"{"V":2,"L":{"t":"h","w":160,"h":60,"x":0,"y":0,"c":[{"t":"p","w":80,"h":60,"x":0,"y":0,"a":true,"i":0,"I":"%0"},{"t":"v","w":79,"h":60,"x":81,"y":0,"c":[{"t":"p","w":79,"h":30,"x":81,"y":0,"i":1,"I":"%1"},{"t":"p","w":79,"h":29,"x":81,"y":31,"i":2,"I":"%2"}]}]}}"#;

    fn leaf(id: &str, width: u64, height: u64, x: i64, y: i64) -> Cell {
        Cell {
            width,
            height,
            x,
            y,
            content: Content::Pane(id.into()),
        }
    }

    fn captured() -> Layout {
        Layout {
            root: Cell {
                width: 160,
                height: 60,
                x: 0,
                y: 0,
                content: Content::Columns(vec![
                    leaf("%0", 80, 60, 0, 0),
                    Cell {
                        width: 79,
                        height: 60,
                        x: 81,
                        y: 0,
                        content: Content::Rows(vec![
                            leaf("%1", 79, 30, 81, 0),
                            leaf("%2", 79, 29, 81, 31),
                        ]),
                    },
                ]),
            },
            floating: Vec::new(),
        }
    }

    fn fixture(shared: bool) -> Layout {
        let y = if shared { 11 } else { 0 };
        let mut members = vec![
            leaf("%1", 59, 2, 41, y),
            leaf("%2", 59, 2, 41, y + 3),
            leaf("%3", 59, 24, 41, y + 6),
        ];
        if shared {
            members.insert(0, leaf("%9", 59, 10, 41, 0));
        }
        let height = 30 + y as u64;
        Layout {
            root: Cell {
                width: 100,
                height,
                x: 0,
                y: 0,
                content: Content::Columns(vec![
                    leaf("%0", 40, height, 0, 0),
                    Cell {
                        width: 59,
                        height,
                        x: 41,
                        y: 0,
                        content: Content::Rows(members),
                    },
                ]),
            },
            floating: Vec::new(),
        }
    }

    fn ids(values: &[&str]) -> Vec<String> {
        values.iter().map(|id| (*id).into()).collect()
    }

    #[test]
    fn reads_legacy_and_json_and_rejects_malformed_layouts() {
        for input in [LEGACY, JSON] {
            assert_eq!(Layout::parse(input), Some(captured()), "{input}");
        }
        for input in [
            "",
            "aabb,",
            "aabb,80x24,0,0,0x",
            "aabb,80x24,0,0[]",
            "aabb,80x24,0,0,0<>",
            r#"{"V":3,"L":{}}"#,
            r#"{"V":2,"L":{"t":"p","w":80,"h":24,"x":0,"y":0,"I":"oops"}}"#,
        ] {
            assert_eq!(Layout::parse(input), None, "{input}");
        }
    }

    #[test]
    fn reads_37c_floating_suffix_and_excludes_its_duplicate_leaves() {
        // The same capture used by the presence consumer's regression.
        let parsed = Layout::parse(
            "0bcf,200x50,0,0{100x50,0,0,0,99x50,101,0,1,40x10,4,2,2,30x8,8,4,3}<30x8,8,4,3,40x10,4,2,2>",
        );
        assert_eq!(
            parsed.as_ref().map(|layout| layout
                .leaves()
                .iter()
                .filter_map(|cell| cell.pane_id())
                .collect::<Vec<_>>()),
            Some(vec!["%0", "%1"]),
        );
        assert_eq!(
            parsed.map(|layout| layout.floating),
            Some(ids(&["%3", "%2"]))
        );
        let json = Layout::parse(r#"{"V":2,"L":{"t":"h","w":80,"h":24,"x":0,"y":0,"c":[{"t":"p","w":80,"h":24,"x":0,"y":0,"I":"%0"},{"t":"p","w":20,"h":10,"x":-2,"y":3,"I":"%1","z":0}]}}"#).unwrap();
        assert_eq!(json.floating, ids(&["%1"]));
        assert_eq!(
            json.leaves()
                .iter()
                .filter_map(|cell| cell.pane_id())
                .collect::<Vec<_>>(),
            ["%0"]
        );
    }

    #[test]
    fn legacy_writer_matches_tmux_checksum_and_round_trips() {
        let layout = captured();
        assert_eq!(layout.legacy(), LEGACY);
        assert_eq!(Layout::parse(&layout.legacy()), Some(layout));
    }

    #[test]
    fn lifts_right_and_down_in_own_and_shared_row_containers() {
        for shared in [false, true] {
            for direction in [SplitDirection::Right, SplitDirection::Down] {
                let before = fixture(shared);
                let order = if shared {
                    ids(&["%0", "%9", "%1", "%2", "%3"])
                } else {
                    ids(&["%0", "%1", "%2", "%3"])
                };
                let result = before.beside_stack(
                    &order,
                    &ids(&["%1", "%2", "%3"]),
                    Some("%3"),
                    "%4",
                    direction,
                );
                let y = if shared { 11 } else { 0 };
                let (width, open_height, newcomer) = match direction {
                    SplitDirection::Right => (29, 24, leaf("%4", 29, 30, 71, y)),
                    SplitDirection::Down => (59, 9, leaf("%4", 59, 14, 41, y + 16)),
                };
                let run = Cell {
                    width,
                    height: if direction == SplitDirection::Down {
                        15
                    } else {
                        30
                    },
                    x: 41,
                    y,
                    content: Content::Rows(vec![
                        leaf("%1", width, 2, 41, y),
                        leaf("%2", width, 2, 41, y + 3),
                        leaf("%3", width, open_height, 41, y + 6),
                    ]),
                };
                let replacement = Cell {
                    width: 59,
                    height: 30,
                    x: 41,
                    y,
                    content: match direction {
                        SplitDirection::Right => Content::Columns(vec![run, newcomer]),
                        SplitDirection::Down => Content::Rows(vec![run, newcomer]),
                    },
                };
                let mut expected = before.clone();
                let Content::Columns(columns) = &mut expected.root.content else {
                    unreachable!()
                };
                if shared {
                    let Content::Rows(rows) = &mut columns[1].content else {
                        unreachable!()
                    };
                    rows.splice(1.., [replacement]);
                } else {
                    columns[1] = replacement;
                }
                assert_eq!(result, Ok(expected), "{shared}, {direction:?}");
            }
        }
    }

    #[test]
    fn refuses_floating_reordered_broken_nested_and_too_short_stacks() {
        let order = ids(&["%0", "%1", "%2", "%3"]);
        let members = ids(&["%1", "%2", "%3"]);
        let mut floating = fixture(false);
        floating.floating.push("%8".into());
        let mut nested = fixture(false);
        let Content::Columns(columns) = &mut nested.root.content else {
            unreachable!()
        };
        let Content::Rows(rows) = &mut columns[1].content else {
            unreachable!()
        };
        rows[1].content = Content::Columns(vec![leaf("%2", 59, 2, 41, 3)]);
        let mut short = fixture(false);
        short.root.height = 16;
        let Content::Columns(columns) = &mut short.root.content else {
            unreachable!()
        };
        columns[0].height = 16;
        columns[1].height = 16;
        let Content::Rows(rows) = &mut columns[1].content else {
            unreachable!()
        };
        rows[2].height = 10;
        for (layout, order, members, direction, reason) in [
            (
                floating,
                order.clone(),
                members.clone(),
                SplitDirection::Right,
                Refusal::Floating,
            ),
            (
                fixture(false),
                ids(&["%1", "%0", "%2", "%3"]),
                members.clone(),
                SplitDirection::Right,
                Refusal::Order,
            ),
            (
                fixture(false),
                order.clone(),
                ids(&["%1", "%3"]),
                SplitDirection::Right,
                Refusal::Members,
            ),
            (
                nested,
                order.clone(),
                members.clone(),
                SplitDirection::Right,
                Refusal::Members,
            ),
            (short, order, members, SplitDirection::Down, Refusal::Height),
        ] {
            assert_eq!(
                layout.beside_stack(&order, &members, Some("%3"), "%4", direction),
                Err(reason)
            );
        }
    }

    #[test]
    fn downward_split_requires_width_for_its_horizontal_staging_split() {
        let layout = Layout::parse("0000,2x30,0,0[2x2,0,0,1,2x2,0,3,2,2x24,0,6,3]").unwrap();
        let members = ids(&["%1", "%2", "%3"]);
        for open in [Some("%3"), None] {
            assert_eq!(
                layout.check_stack(&members, &members, open, SplitDirection::Down),
                Err(Refusal::Width),
                "a downward lift still stages a horizontal split"
            );
        }
    }

    #[test]
    fn post_split_preserves_every_leaf_outside_the_staging_split() {
        let before = fixture(false);
        let mut staged = before.clone();
        let Content::Columns(columns) = &mut staged.root.content else {
            unreachable!()
        };
        let Content::Rows(rows) = &mut columns[1].content else {
            unreachable!()
        };
        rows[2].content =
            Content::Columns(vec![leaf("%3", 29, 24, 41, 6), leaf("%4", 29, 24, 71, 6)]);
        assert_eq!(staged.check_split_geometry(&before, "%3", "%4"), Ok(()));
        for changed in [
            leaf("%0", 39, 30, 0, 0),
            leaf("%0", 40, 29, 0, 0),
            leaf("%0", 40, 30, 1, 0),
            leaf("%0", 40, 30, 0, 1),
            leaf("%9", 40, 30, 0, 0),
        ] {
            let mut after = staged.clone();
            let Content::Columns(columns) = &mut after.root.content else {
                unreachable!()
            };
            columns[0] = changed;
            assert_eq!(
                after.check_split_geometry(&before, "%3", "%4"),
                Err(Refusal::Moved),
                "a foreign leaf changed: {after:?}"
            );
        }
        let mut resized = staged.clone();
        resized.root.height += 1;
        let Content::Columns(columns) = &mut resized.root.content else {
            unreachable!()
        };
        columns[0].height += 1;
        assert_eq!(
            resized.check_split_geometry(&before, "%3", "%4"),
            Err(Refusal::Moved)
        );
        let mut shrunk = staged.clone();
        shrunk.root.height -= 1;
        assert_eq!(
            shrunk.check_split_geometry(&before, "%3", "%4"),
            Err(Refusal::Moved),
            "a window shrink taken from the staged member alone"
        );
        let Content::Columns(columns) = &mut staged.root.content else {
            unreachable!()
        };
        let Content::Rows(rows) = &mut columns[1].content else {
            unreachable!()
        };
        rows[0].height += 1;
        assert_eq!(
            staged.check_split_geometry(&before, "%3", "%4"),
            Err(Refusal::Moved),
            "an unsplit stack member changed"
        );
    }

    #[test]
    fn post_split_requires_the_expected_order_and_adjacent_newcomer() {
        let before = ids(&["%0", "%1", "%2", "%3"]);
        let mut layout = fixture(false);
        assert_eq!(
            layout.check_split_order(&before, &before, "%3", "%4"),
            Err(Refusal::Newcomer)
        );
        let Content::Columns(columns) = &mut layout.root.content else {
            unreachable!()
        };
        let Content::Rows(rows) = &mut columns[1].content else {
            unreachable!()
        };
        rows.push(leaf("%4", 59, 1, 41, 31));
        let after = ids(&["%0", "%1", "%2", "%3", "%4"]);
        assert_eq!(
            layout.check_split_order(&after, &before, "%3", "%4"),
            Ok(())
        );
        assert_eq!(
            layout.check_split_order(&ids(&["%0", "%1", "%2", "%4", "%3"]), &before, "%3", "%4"),
            Err(Refusal::Order)
        );
        assert_eq!(
            layout.check_split_order(&after, &before, "%2", "%4"),
            Err(Refusal::Newcomer)
        );
    }
}
