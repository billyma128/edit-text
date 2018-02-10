use bus::Bus;
use failure::Error;
use oatie::OT;
use oatie::doc::*;
use oatie::parse::debug_pretty;
use oatie::schema::RtfSchema;
use oatie::transform::transform;
use oatie::validate::{validate_doc_span, ValidateContext};
use serde_json;
use std::{panic, process};
use std::collections::{HashSet, HashMap};
use std::sync::{Arc, Mutex};
use std::thread;
use std::time::Duration;
use crate::{SyncClientCommand, SyncServerCommand};
use ws;
use std::collections::VecDeque;
use std::fs::File;
use std::io::prelude::*;
use std::path::Path;
use ron;

lazy_static! {
    static ref LOG_SYNC_FILE: Arc<Mutex<File>> = {
        let path = Path::new("./log/server");
        Arc::new(Mutex::new(File::create(path).unwrap()))
    };
}

#[derive(Debug, Serialize, Deserialize)]
pub enum LogSync {
    Launch,
    ServerSpawn,
    ClientConnect,
    ClientPacket(SyncServerCommand),
    Debug(String),
    Spawn,
}

// Macros can only be used after they are defined
macro_rules! log_sync {
    ( $x:expr ) => {
        {
            // use $crate::wasm::LogWasm::*;
            let mut file_guard = LOG_SYNC_FILE.lock().unwrap();
            use $crate::sync::LogSync::*;
            writeln!(*file_guard, "{}", ron::ser::to_string(&$x).unwrap());
            let _ = file_guard.sync_data();
        }
    };
}

pub fn default_doc() -> Doc {
    Doc(doc_span![
        DocGroup({"tag": "h1"}, [
            // DocGroup({"tag": "caret", "client": "left"}, []),
            // DocGroup({"tag": "caret", "client": "right"}, []),
            DocChars("Hello world!"),
        ]),
        DocGroup({"tag": "p"}, [
            // DocChars("What's "),
            // DocGroup({"tag": "span", "class": "bold"}, [DocChars("new and great")]),
            // DocChars(" with you?"),
            DocChars("This is Mercutio, a rich text editor."),
        ]),
        // DocGroup({"tag": "ul"}, [
        //     DocGroup({"tag": "li"}, [
        //         DocGroup({"tag": "p"}, [
        //             DocChars("Three adjectives strong."),
        //         ]),
        //         DocGroup({"tag": "p"}, [
        //             DocChars("World!"),
        //         ]),
        //     ]),
        // ])
    ])
}

#[derive(Clone)]
pub struct MoteState {
    pub body: Arc<Mutex<Doc>>,
}

pub fn action_sync(doc: &Doc, ops_a: Vec<Op>, ops_b: Vec<Op>) -> Result<(Doc, Op), Error> {
    println!(" ---> input ops_a");
    println!("{:?}", ops_a);
    println!();

    // Flatten client A operations.
    let mut op_a = op_span!([], []);
    for op in &ops_a {
        op_a = OT::compose(&op_a, op);
    }

    println!(" ---> input ops_b");
    println!("{:?}", ops_b);
    println!();

    // Flatten client B operations.
    let mut op_b = op_span!([], []);
    for op in &ops_b {
        op_b = OT::compose(&op_b, op);
    }

    println!("OP A {:?}", op_a);
    println!("OP B {:?}", op_b);

    let test = format!(
        r#"
doc:   {}

a_del: {}
a_add: {}

b_del: {}
b_add: {}
"#,
        debug_pretty(&doc.0),
        debug_pretty(&op_a.0),
        debug_pretty(&op_a.1),
        debug_pretty(&op_b.0),
        debug_pretty(&op_b.1)
    );

    // TODO dump to document
    {
        use std::io::prelude::*;
        let mut f = ::std::fs::File::create("test.txt").unwrap();
        f.write_all(&test.as_bytes()).unwrap();
        f.sync_all().unwrap();
    }

    println!();
    println!("<test>");
    print!("{}", test);
    println!("</test>");
    println!();

    println!("(!) recreating initial client state...");
    println!();

    // TODO remove this validation code if we're performing the check client-side

    // let mut check_op_a = op_span!([], []);
    // for (i, op) in ops_a.iter().enumerate() {
    //     println!("  A: applying {:?}/{:?}", i + 1, ops_a.len());
    //     check_op_a = OT::compose(&check_op_a, &op);
    //     println!(" op: {}", debug_pretty(&check_op_a));
    //     let _ = OT::apply(&doc.clone(), &check_op_a);
    // }

    // println!();

    // let mut check_op_b = op_span!([], []);
    // for (i, op) in ops_b.iter().enumerate() {
    //     println!("  B: applying {:?}/{:?}", i + 1, ops_b.len());
    //     check_op_b = OT::compose(&check_op_b, &op);
    //     println!(" op: {}", debug_pretty(&check_op_b));
    //     let _ = OT::apply(&doc.clone(), &check_op_b);
    // }

    let doc_a = OT::apply(&doc.clone(), &op_a);
    let doc_b = OT::apply(&doc.clone(), &op_b);

    println!("ok");
    println!();

    println!("(!) applying transformed operations...");

    // Tranform
    let (a_, b_) = transform::<RtfSchema>(&op_a, &op_b);

    println!("");
    println!("DOC A {:?}", doc_a);
    println!("OP A' {:?}", a_);
    let a_res = OT::apply(&doc_a, &a_);

    println!("");
    println!("DOC B {:?}", doc_b);
    println!("OP B' {:?}", b_);
    let b_res = OT::apply(&doc_b, &b_);

    println!("");
    println!("a res {:?}", a_res);
    println!("b res {:?}", b_res);

    println!("equal? {:?}", a_res == b_res);

    let success = if a_res != b_res { false } else { true };

    // TODO return error when success is false

    let new_doc = Doc(a_res.0);
    let mut validate_ctx = ValidateContext::new();
    validate_doc_span(&mut validate_ctx, &new_doc.0).expect("Validation error");

    Ok((new_doc, OT::compose(&op_a, &a_)))
}

pub struct SyncState {
    ops: VecDeque<(String, usize, Op)>,
    version: usize,
    history: HashMap<usize, Op>,
}

/// Transform an operation incrementally against each interim document operation.
pub fn update_operation(mut op: Op, history: &HashMap<usize, Op>, target_version: usize, mut input_version: usize) -> Op {
    // Transform against each interim operation.
    // TODO upgrade_operation_to_current or something
    while input_version < target_version {
        // If the version exists (it should) transform against it.
        if let Some(ref version_op) = history.get(&input_version) {
            let (updated_op, _) = Op::transform::<RtfSchema>(version_op, &op);
            op = updated_op;
        }

        input_version += 1;
    }
    op  
}

pub fn sync_socket_server(port: u16, period: usize, state: MoteState) {
    thread::spawn(move || {
        log_sync!(Spawn);

        let url = format!("0.0.0.0:{}", port);

        println!("Listening sync_socket_server on 0.0.0.0:{}", port);

        let sync_state_mutex = Arc::new(Mutex::new(SyncState {
            ops: VecDeque::new(),
            version: 100,
            history: hashmap![],
        }));

        let bus = Arc::new(Mutex::new(Bus::new(255)));

        let sync_state_mutex_capture = sync_state_mutex.clone();
        let state_capture = state.clone();
        let bus_capture = bus.clone();
        thread::spawn(move || {
            loop {
                // Wait a set duration between transforms.
                thread::sleep(Duration::from_millis(period as u64));

                let mut sync_state = sync_state_mutex_capture.lock().unwrap();

                let mut doc = state_capture.body.lock().unwrap();

                // Go through the deque and update our operations.
                while let Some((client_id, input_version, op)) = sync_state.ops.pop_front() {
                    let target_version = sync_state.version;

                    log_sync!(Debug(format!("{:?}", op)));
                    
                    // let res = action_sync(&doc, new_op, op_group).unwrap();

                    // Update the operation so we can apply it to the document.
                    let op = update_operation(op, &sync_state.history, target_version, input_version);
                    sync_state.history.insert(target_version, op.clone());

                    // Update the document with this operation.
                    *doc = Op::apply(&doc, &op);
                    sync_state.version = target_version + 1;

                    // Broadcast to all connected websockets.
                    bus_capture
                        .lock()
                        .unwrap()
                        .broadcast((doc.0.clone(), sync_state.version, client_id, op));
                }
            }
        });

        let state_capture = state.clone();
        let bus_capture = bus.clone();
        ws::listen(url, move |out| {
            log_sync!(ClientConnect);

            // Initial document state.
            {
                let doc = state.body.lock().unwrap();
                let mut sync_state = sync_state_mutex.lock().unwrap();
                let command = SyncClientCommand::Update(doc.0.clone(), sync_state.version, "$sync".to_string(), OT::empty());
                out.send(serde_json::to_string(&command).unwrap());
            }

            let mut rx = { bus_capture.lock().unwrap().add_rx() };
            thread::spawn(move || {
                while let Ok((doc, version, client_id, op)) = rx.recv() {
                    let command = SyncClientCommand::Update(doc, version, client_id, op);
                    out.send(serde_json::to_string(&command).unwrap());
                }
            });

            let state = state.clone();
            let sync_state_mutex_capture = sync_state_mutex.clone();
            move |msg: ws::Message| {
                let req_parse: Result<SyncServerCommand, _> =
                    serde_json::from_slice(&msg.into_data());
                match req_parse {
                    Ok(value) => {
                        log_sync!(ClientPacket(value.clone()));

                        match value {
                            SyncServerCommand::Commit(client_id, op, version) => {
                                let mut sync_state = sync_state_mutex_capture.lock().unwrap();
                                sync_state.ops.push_back((client_id, version, op));
                            }
                        }
                    }
                    Err(err) => {
                        println!("Packet error: {:?}", err);
                    }
                }

                Ok(())
            }
        })
    });
}