mod styles;

pub use self::styles::*;
use crate::walkers::*;
use failure::Error;
use oatie::doc::*;
use oatie::schema::RtfSchema;
use oatie::OT;
use oatie::style::OpaqueStyleMap;
use std::collections::HashSet;

fn is_boundary_char(c: char) -> bool {
    c.is_whitespace() || c == '-' || c == '_'
}

#[derive(Clone)]
pub struct ActionContext {
    pub doc: Doc,
    pub client_id: String,
    op_result: Op,
}

impl ActionContext {
    pub fn new(doc: Doc, client_id: String) -> ActionContext {
        ActionContext {
            doc,
            client_id,
            op_result: Op::empty(),
        }
    }

    pub fn apply(mut self, op: &Op) -> Result<ActionContext, Error> {
        // update self with the op, update self doc, return new self
        self.doc = Op::apply(&self.doc, op);
        self.op_result = Op::compose(&self.op_result, op);
        Ok(self)
    }

    pub fn get_walker<'a>(&'a self, pos: Pos) -> Result<Walker<'a>, Error> {
        Walker::to_caret(&self.doc, &self.client_id, pos)
    }

    pub fn result(self) -> Op {
        self.op_result
    }
}

pub fn init_caret(ctx: ActionContext) -> Result<Op, Error> {
    let mut walker = Walker::new(&ctx.doc);
    if !walker.goto_pos(0) {
        bail!("Could not insert first caret");
    }

    let mut writer = walker.to_writer();
    writer.add.begin();
    writer.add.close(hashmap! {
        "tag".to_string() => "caret".to_string(),
        "client".to_string() => ctx.client_id.clone(),
        "focus".to_string() => "true".to_string(),
    });
    writer.add.begin();
    writer.add.close(hashmap! {
        "tag".to_string() => "caret".to_string(),
        "client".to_string() => ctx.client_id.clone(),
        "focus".to_string() => "false".to_string(),
    });
    Ok(writer.exit_result())
}

pub fn add_string(ctx: ActionContext, input: &str) -> Result<ActionContext, Error> {
    Ok(ctx)
        .and_then(delete_selection)
        .and_then(|(_success, ctx)| {
            // Insert before start caret (given the carets are now collapsed).
            let walker = ctx.get_walker(Pos::Start)?;

            // Clone styles of hte previous text node, or use default styles.
            let mut styles = hashmap!{ Style::Normie => None };
            let mut char_walker = walker.clone();
            char_walker.back_char();
            if let Some(DocChars(_, ref prefix_styles)) = char_walker.doc().head() {
                styles.extend(
                    prefix_styles
                        .iter()
                        .map(|(a, b)| (a.to_owned(), b.to_owned())),
                );
            }

            // Insert new character.
            let mut writer = walker.to_writer();
            let step = AddChars(DocString::from_str(input), OpaqueStyleMap::from(styles));
            writer.add.place(&step);
            ctx.apply(&writer.exit_result())
        })
}

pub fn toggle_list(ctx: ActionContext) -> Result<Op, Error> {
    // Create a walker that points to the beginning of the block the caret
    // is currently in.
    let mut walker = ctx.get_walker(Pos::Focus).expect("Expected a Focus caret");
    assert!(walker.back_block());

    // If the parent of our current block is a list item, delete it.
    let mut parent_walker = walker.clone();
    if parent_walker.parent() {
        if let Some(DocGroup(ref attrs, ref span)) = parent_walker.doc().head() {
            if attrs["tag"] == "bullet" {
                // Delete the bullet group.
                let mut writer = parent_walker.to_writer();
                writer
                    .del
                    .place(&DelGroup(del_span![DelSkip(span.skip_len())]));
                return Ok(writer.exit_result());
            }
        }
    }

    // Wrap current block with a bullet group.
    Ok({
        let mut writer = walker.to_writer();
        writer.add.place(&AddGroup(
            hashmap! { "tag".to_string() => "bullet".to_string() },
            add_span![AddSkip(1)],
        ));
        writer.exit_result()
    })
}

#[derive(Debug, Clone)]
pub struct CaretState {
    pub block: String,
    pub in_list: bool,
    pub styles: HashSet<Style>,
}

// Return a "caret state".
// Returns block tag, whether we are in a list, and more. 
pub fn identify_block(ctx: ActionContext) -> Result<CaretState, Error> {
    // Identify selection styles.
    let styles = identify_styles(&ctx)?;

    let mut walker = ctx.get_walker(Pos::Focus)?;
    assert!(walker.back_block());
    if let Some(DocGroup(ref attrs, _)) = walker.doc().head() {
        let tag = attrs["tag"].clone();
        let mut in_list = false;
        if walker.parent() {
            if let Some(DocGroup(ref attrs_2, _)) = walker.doc().head() {
                in_list = attrs_2["tag"] == "bullet";
            }
        }
        Ok(CaretState {
            block: tag,
            in_list,
            styles,
        })
    } else {
        bail!("Expected a DocGroup from back_block");
    }
}

/// Replaces the current block with a new block.
pub fn replace_block(ctx: ActionContext, tag: &str) -> Result<Op, Error> {
    // Create a walker that points to the beginning of the block the caret
    // is currently in.
    let mut walker = ctx.get_walker(Pos::Focus).expect("Expected a Focus caret");
    assert!(walker.back_block());

    // Get the skip length of the contents of the current block.
    let len = match walker.doc().head() {
        Some(DocGroup(_, ref span)) => span.skip_len(),
        _ => unreachable!(),
    };

    // Delete the current block, and the wrap its contents with a new block.
    Ok({
        let mut writer = walker.to_writer();
        writer.del.place(&DelGroup(
            del_span![DelSkip(len)],
        ));
        writer.add.place_all(&add_span![AddGroup(
            {"tag": tag.to_string()},
            [AddSkip(len)],
        )]);
        writer.exit_result()
    })
}

/// Hit backspace at the beginning of a block.
fn combine_with_previous_block(
    mut block_walker: Walker<'_>,
    mut parent_walker: Walker<'_>,
) -> Result<Op, Error> {
    // Check for first block in a list item.
    assert!(parent_walker.back_block());

    // Check if we are in a block inside of a list item. Also get the length
    // of the contents of the parent list item, or otherwise just use "1"
    // (indicating the current block).
    let mut is_list_item = false;
    let mut list_item_skip_len = 1;
    if parent_walker.doc().unhead() == None && parent_walker.parent() {
        if let Some(DocGroup(ref attrs_2, ref span_2)) = parent_walker.doc().head() {
            if attrs_2["tag"] == "bullet" {
                // We are at the start of a block inside of a list item.
                is_list_item = true;
                list_item_skip_len = span_2.skip_len();
            }
        }
    }

    // Check if parent is preceded by a list item.
    // 1. If we are in a list item also, delete both and join together as one
    //    list item.
    // 2. If we are not in a list item, delete previous group and create a new
    //    group spanning its contents and our block.
    // contents of both list items.
    if let Some(DocGroup(ref attrs, ref span)) = parent_walker.doc().unhead() {
        if attrs["tag"] == "bullet" {
            // Create local copies of attributes and span length of the previous
            // bullet group.
            let attrs = attrs.to_owned();
            let skip_len = span.skip_len();

            // Move to preceding bullet.
            parent_walker.stepper.doc.prev();

            return Ok({
                let mut writer = parent_walker.to_writer();

                writer.del.begin();
                if skip_len > 0 {
                    writer.del.place(&DelSkip(skip_len));
                }
                writer.del.close();
                if is_list_item {
                    writer.del.begin();
                }
                if list_item_skip_len > 0 {
                    writer.del.place(&DelSkip(list_item_skip_len));
                }
                if is_list_item {
                    writer.del.close();
                }

                writer.add.begin();
                if skip_len + list_item_skip_len > 0 {
                    writer
                        .add
                        .place(&AddSkip(skip_len + list_item_skip_len));
                }
                writer.add.close(attrs);

                writer.exit_result()
            });
        }
    } else {
        // We think we're at the start of the document, so do nothing.
        return Ok(Op::empty());
    }

    // If we are in a list item but there is no preceding list item, unindent
    // the current list item by deleting it and preserving its contents.
    if is_list_item {
        return Ok({
            let mut writer = parent_walker.to_writer();
            writer.del.begin();
            if list_item_skip_len > 0 {
                writer.del.place(&DelSkip(list_item_skip_len));
            }
            writer.del.close();
            writer.exit_result()
        });
    }

    // Return to block parent.
    assert!(block_walker.back_block());
    let span_2 = match block_walker.stepper().head() {
        Some(DocGroup(.., span)) => span.skip_len(),
        _ => unreachable!(),
    };

    // Move to prior block to join it, or abort.
    if !block_walker.back_block_or_block_object() {
        return Ok(op_span!([], []));
    }

    // If block is an "hr", delete it.
    if let Some(DocGroup(ref attrs, _)) = block_walker.doc().head() {
        if attrs["tag"] == "hr" {
            // Remove horizontal rule.
            return Ok({
                let mut writer = block_walker.to_writer();
                writer.del.begin();
                writer.del.close();
                writer.exit_result()
            });
        }
    } else {
        unreachable!();
    }

    // Get the length and attributes of the previous block.
    let (attrs, span_1) = match block_walker.stepper().head() {
        Some(DocGroup(attrs, span)) => (attrs, span.skip_len()),
        _ => unreachable!(),
    };

    Ok({
        let mut writer = block_walker.to_writer();

        // Delete both blocks.
        writer.del.begin();
        if span_1 > 0 {
            writer.del.place(&DelSkip(span_1));
        }
        writer.del.close();
        writer.del.begin();
        if span_2 > 0 {
            writer.del.place(&DelSkip(span_2));
        }
        writer.del.close();

        // Surround both block contents with a single block.
        writer.add.begin();
        if span_1 + span_2 > 0 {
            writer.add.place(&AddSkip(span_1 + span_2));
        }
        writer.add.close(attrs.to_owned());

        writer.exit_result()
    })
}

// Deletes backward once from a provided walker position.
fn delete_char_inner(mut walker: Walker<'_>) -> Result<Op, Error> {
    // Check if caret is at the start of a block.
    let mut block_walker = walker.clone();
    assert!(block_walker.back_block());
    // console_log!("before at start of block {:?} vs {:?}", walker.caret_pos(), block_walker.caret_pos());
    block_walker.stepper.doc.enter();
    let at_start_of_block = walker.caret_pos() == block_walker.caret_pos();
    // console_log!("at start of block {:?} vs {:?}", walker.caret_pos(), block_walker.caret_pos());

    // See if we can collapse this and the previous block or list item.
    if at_start_of_block {
        return combine_with_previous_block(block_walker, walker);
    }

    walker.back_char();

    // console_log!("back char {:?}", walker);

    // Skip past adjacent carets in between cursor and the next char.
    // TODO is there a more elegant way to do this:
    while let Some(DocGroup(ref attrs, _)) = walker.doc().head() {
        if attrs["tag"] == "caret" {
            walker.stepper.doc.next();
        }
    }

    // Check that we precede a character.
    if let Some(DocChars(..)) = walker.doc().head() {
        // fallthrough
    } else {
        // Check if parent is span, if so move outside span
        // TODO check that the parent is actually a span
        // TODO this might not be possible anymore without spans.
        walker.stepper.next();
        if let Some(DocChars(..)) = walker.doc().head() {
            // fallthrough
        } else {
            return Ok(op_span!([], []));
        }
    }

    // Delete the character.
    let mut writer = walker.to_writer();
    writer.del.place(&DelChars(1));
    Ok(writer.exit_result())
}

/// Deletes the contents of the current selection. Returns a modified context
/// and a boolean indicating if a selection existed to delete.
fn delete_selection(ctx: ActionContext) -> Result<(bool, ActionContext), Error> {
    Ok(ctx)
        .and_then(|ctx| {
            let start = ctx.get_walker(Pos::Start)?;
            let end = ctx.get_walker(Pos::End)?;
            let delta = end.delta(&start).unwrap_or(0) as usize;

            // If we found a selection, delete every character in the selection.
            // We implement this by looping until the caret distance between our
            // cursors is 0.
            // TODO: This is incredibly inefficient.
            //  1. Dont' recurse infinitely, do this in a loop.
            //  2. Skip entire DocChars components instead of one character at a time.
            Ok(
                if delta != 0 {
                    // Get real weird with it.
                    let op = delete_char_inner(end)?;
                    let ctx = ctx.apply(&op)?;
                    if delta > 1 {
                        delete_selection(ctx)?
                    } else {
                        (true, ctx)
                    }
                } else {
                    (false, ctx)
                }
            )
        })
}

/// Backspace.
pub fn delete_char(ctx: ActionContext) -> Result<Op, Error> {
    // Bail early if we delete a selection.
    let (success, ctx) = delete_selection(ctx)?;
    if success {
        return Ok(ctx.result());
    }

    // Fallback; delete backward from start caret (given the carets are collapsed).
    let walker = ctx.get_walker(Pos::Start)?;
    delete_char_inner(walker)
}

// Splits the current block at the position of the user's caret.
pub fn split_block(ctx: ActionContext, add_hr: bool) -> Result<Op, Error> {
    let walker = ctx.get_walker(Pos::Focus).expect("Expected a Focus caret");
    let skip = walker.doc().skip_len();

    // Identify the tag of the block we're splitting.
    let mut prev_walker = walker.clone();
    assert!(prev_walker.back_block());
    let previous_block = if let Some(DocGroup(attrs, _)) = prev_walker.doc().head() {
        attrs["tag"].to_string()
    } else {
        unreachable!();
    };

    // Identify if we're nested inside of a bullet.
    let mut parent_walker = prev_walker.clone();
    let mut nested_bullet = false;
    if parent_walker.parent() {
        if let Some(DocGroup(ref attrs, _)) = parent_walker.doc().head() {
            if attrs["tag"] == "bullet" {
                nested_bullet = true;
            }
        }
    }

    Ok({
        let mut writer = walker.to_writer();

        if skip > 0 {
            writer.del.place(&DelSkip(skip));
        }
        writer.del.close();
        if nested_bullet {
            writer.del.close();
        }

        writer
            .add
            .close(hashmap! { "tag".into() => previous_block });
        if nested_bullet {
            writer
                .add
                .close(hashmap! { "tag".into() => "bullet".into() });
            writer.add.begin();
        }
        if add_hr {
            writer.add.begin();
            writer.add.close(hashmap! { "tag".into() => "hr".into() });
        }
        writer.add.begin();
        if skip > 0 {
            writer.add.place(&AddSkip(skip));
        }
        writer.add.close(hashmap! { "tag".into() => "p".into() });
        if nested_bullet {
            writer
                .add
                .close(hashmap! { "tag".into() => "bullet".into() });
        }

        writer.exit_result()
    })
}

/// Arrow keys move the caret.
pub fn caret_move(
    ctx: ActionContext,
    increase: bool,
    preserve_select: bool,
) -> Result<Op, Error> {
    Ok(ctx)
        .and_then(|ctx| {
            // If we aren't preserving the selection, collapse the anchor caret
            // to where the focus caret is.
            if !preserve_select {
                let op = caret_clear_optional(&ctx, Pos::Anchor);
                ctx.apply(&op)
            } else {
                Ok(ctx)
            }
        })
        .and_then(|ctx| {
            let mut walker = ctx.get_walker(Pos::Focus)?;

            // Remove focus caret and move it to next position.
            let op = Op::transform_advance::<RtfSchema>(&{
                // First operation removes the caret.
                let mut writer = walker.to_writer();
                writer.del.begin();
                writer.del.close();
                writer.exit_result()
            }, &{
                // Move the walker to the new position.
                if increase {
                    walker.next_char();
                } else {
                    walker.back_char();
                }

                // Insert the carets.
                let mut writer = walker.to_writer();
                if !preserve_select {
                    writer.add.begin();
                    writer.add.close(hashmap! {
                        "tag".to_string() => "caret".to_string(),
                        "client".to_string() => ctx.client_id.clone(),
                        "focus".to_string() => "false".to_string(),
                    });
                }
                writer.add.begin();
                writer.add.close(hashmap! {
                    "tag".to_string() => "caret".to_string(),
                    "client".to_string() => ctx.client_id.clone(),
                    "focus".to_string() => "true".to_string(),
                });
                writer.exit_result()
            });

            ctx.apply(&op)
        })
        .map(|ctx| ctx.result())
}

pub fn caret_word_move(ctx: ActionContext, increase: bool) -> Result<Op, Error> {
    Ok(ctx)
        .and_then(|ctx| {
            let op = caret_clear_optional(&ctx, Pos::Anchor);
            ctx.apply(&op)
        })
        .and_then(|ctx| {
            let mut walker = ctx.get_walker(Pos::Focus).expect("Expected a Focus caret");

            // First operation removes the caret.
            let mut writer = walker.to_writer();
            writer.del.begin();
            writer.del.close();
            let op_1 = writer.exit_result();

            // Find the next walker position after the current word.
            if increase {
                walker.next_char();
                loop {
                    match walker.doc().head() {
                        Some(DocChars(ref text, _)) => {
                            if is_boundary_char(text.as_str().chars().next().unwrap()) {
                                break;
                            } else {
                                walker.next_char();
                            }
                        }
                        Some(DocGroup(ref attrs, _)) => {
                            if attrs["tag"] == "caret" {
                                // guess we'll stop
                                break;
                            }
                        }
                        None => {
                            // guess we'll stop
                            break;
                        }
                    }
                }
            } else {
                // Move backward.
                walker.back_char();
                loop {
                    match walker.doc().unhead() {
                        Some(DocChars(ref text, _)) => {
                            if is_boundary_char(text.as_str().chars().rev().next().unwrap()) {
                                break;
                            } else {
                                walker.back_char();
                            }
                        }
                        Some(DocGroup(ref attrs, _)) => {
                            if attrs["tag"] == "caret" {
                                // guess we'll stop
                                break;
                            }
                        }
                        None => {
                            // guess we'll stop
                            break;
                        }
                    }
                }
            }

            // Second operation inserts the new caret.
            let mut writer = walker.to_writer();
            writer.add.begin();
            writer.add.close(hashmap! {
                "tag".to_string() => "caret".to_string(),
                "client".to_string() => ctx.client_id.clone(),
                "focus".to_string() => "true".to_string(),
            });
            writer.add.begin();
            writer.add.close(hashmap! {
                "tag".to_string() => "caret".to_string(),
                "client".to_string() => ctx.client_id.clone(),
                "focus".to_string() => "false".to_string(),
            });
            let op_2 = writer.exit_result();

            // Return composed operations. Select proper order or otherwise composition
            // will be invalid.
            ctx.apply(&Op::transform_advance::<RtfSchema>(&op_1, &op_2))
        })
        .map(|ctx| ctx.result())
}

pub fn caret_select_all(ctx: ActionContext) -> Result<Op, Error> {
    Ok(Op::transform_advance::<RtfSchema>(&{
        Op::transform_advance::<RtfSchema>(&{
            // Delete focus caret.
            caret_clear(ctx.clone(), Pos::Focus)
                .map(|(_pos_1, op_1)| op_1)
                .unwrap_or_else(|_| Op::empty())
        }, &{
            // Delete anchor caret.
            caret_clear(ctx.clone(), Pos::Anchor)
                .map(|(_pos_1, op_1)| op_1)
                .unwrap_or_else(|_| Op::empty())
        })
    }, &{
        Op::transform_advance::<RtfSchema>(&{
            // Insert anchor caret at start.
            let mut start = Walker::new(&ctx.doc);
            start.goto_pos(0);

            let mut writer = start.to_writer();
            writer.add.begin();
            writer.add.close(hashmap! {
                "tag".to_string() => "caret".to_string(),
                "client".to_string() => ctx.client_id.clone(),
                "focus".to_string() => "false".to_string(),
            });
            writer.exit_result()
        }, &{
            // Insert focus caret at end.
            let mut end = Walker::new(&ctx.doc);
            end.goto_end();

            let mut writer = end.to_writer();
            writer.add.begin();
            writer.add.close(hashmap! {
                "tag".to_string() => "caret".to_string(),
                "client".to_string() => ctx.client_id.clone(),
                "focus".to_string() => "true".to_string(),
            });
            writer.exit_result()
        })
    }))
}

pub fn caret_block_move(ctx: ActionContext, increase: bool) -> Result<Op, Error> {
    let mut walker = ctx.get_walker(Pos::Focus).expect("Expected a Focus caret");

    // First operation removes the caret.
    let mut writer = walker.to_writer();
    writer.del.begin();
    writer.del.close();
    let op_1 = writer.exit_result();

    // Second operation inserts the new caret.
    if increase {
        if !walker.next_block() {
            return Ok(op_span!([], []));
        }
    } else {
        assert!(walker.back_block());
        let _ = walker.back_block(); // don't care
    }

    let mut writer = walker.to_writer();
    writer.add.begin();
    writer.add.close(hashmap! {
        "tag".to_string() => "caret".to_string(),
        "client".to_string() => ctx.client_id.clone(),
        "focus".to_string() => "false".to_string(),
    });
    writer.add.begin();
    writer.add.close(hashmap! {
        "tag".to_string() => "caret".to_string(),
        "client".to_string() => ctx.client_id.clone(),
        "focus".to_string() => "true".to_string(),
    });
    let op_2 = writer.exit_result();

    // Return composed operations. Select proper order or otherwise composition
    // will be invalid.
    Ok(Op::transform_advance::<RtfSchema>(&op_1, &op_2))
}

// Delete a caret, return its position.
pub fn caret_clear_inner(walker: Walker<'_>) -> Result<(isize, Op), Error> {
    let pos = walker.caret_pos();
    let mut writer = walker.to_writer();
    writer.del.begin();
    writer.del.close();
    Ok((pos, writer.exit_result()))
}

// Deletes a caret, returning its position.
pub fn caret_clear(ctx: ActionContext, position: Pos) -> Result<(isize, Op), Error> {
    let walker = ctx.get_walker(position)?;
    caret_clear_inner(walker)
}

fn caret_clear_optional(ctx: &ActionContext, pos: Pos) -> Op {
    caret_clear(ctx.clone(), pos)
        .map(|(_pos, op)| op)
        .unwrap_or(Op::empty())
}

pub fn cur_to_caret(ctx: ActionContext, cur: &CurSpan, focus: bool) -> Result<Op, Error> {
    Ok(Op::transform_advance::<RtfSchema>(&{
        // First operation removes the caret.
        caret_clear(ctx.clone(), if focus { Pos::Focus } else { Pos::Anchor })
            .map(|(_pos, op)| op)
            .unwrap_or_else(|_| Op::empty())
    }, &{
        // Second operation inserts a new caret.
        let walker = Walker::to_cursor(&ctx.doc, cur);
        let mut writer = walker.to_writer();
        writer.add.begin();
        writer.add.close(hashmap! {
            "tag".to_string() => "caret".to_string(),
            "client".to_string() => ctx.client_id.clone(),
            "focus".to_string() => if focus { format!("true") } else { format!("false") },
        });
        writer.exit_result()
    }))
}
