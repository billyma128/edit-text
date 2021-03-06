use failure::Error;
use oatie::doc::*;
use oatie::rtf::*;
use oatie::writer::*;

fn remove_carets_span(span: &DocSpan<RtfSchema>) -> Result<DocSpan<RtfSchema>, Error> {
    let mut ret: DocSpan<RtfSchema> = vec![];

    for elem in span {
        match *elem {
            DocGroup(ref attrs, ref span) => {
                if let Attrs::Caret { .. } = attrs {
                    // fall-through
                } else {
                    let res = remove_carets_span(span)?;
                    ret.place(&DocGroup(attrs.clone(), res));
                }
            }
            DocText(..) => {
                ret.place(elem);
            }
        }
    }
    Ok(ret)
}

/// Removes carets from a doc.
// TODO maybe just merge this with the below function
pub fn remove_carets(doc: &Doc<RtfSchema>) -> Result<Doc<RtfSchema>, Error> {
    Ok(Doc(remove_carets_span(&doc.0)?))
}

fn remove_carets_op_span(
    writer: &mut DelWriter<RtfSchema>,
    span: &DocSpan<RtfSchema>,
    filter: &[String],
) -> Result<(), Error> {
    for elem in span {
        match *elem {
            DocGroup(ref attrs, ref span) => {
                if let Attrs::Caret { ref client_id, .. } = attrs {
                    if filter.contains(client_id) {
                        assert!(span.is_empty());
                        writer.begin();
                        writer.close();
                        continue;
                    }
                }

                // else
                writer.begin();
                remove_carets_op_span(writer, span, filter)?;
                writer.exit();
            }
            DocText(_, ref text) => {
                writer.place(&DelSkip(text.char_len()));
            }
        }
    }
    Ok(())
}

/// Removes carets from a doc. Filter contains the client IDs to remove.
pub fn remove_carets_op(doc: &Doc<RtfSchema>, filter: Vec<String>) -> Result<Op<RtfSchema>, Error> {
    let mut writer = DelWriter::new();
    remove_carets_op_span(&mut writer, &doc.0, &filter)?;
    Ok(Op(writer.result(), vec![]))
}
