use std::fs;

#[tokio::test(flavor = "current_thread")]
async fn runs_typescript_with_node_fs() {
    let dir = tempfile::tempdir().unwrap();
    fs::write(dir.path().join("data.json"), r#"{"answer":42}"#).unwrap();
    fs::write(
        dir.path().join("main.ts"),
        r#"
import { readFileSync } from "node:fs";
import { basename } from "node:path";

type Data = { answer: number };
const data: Data = JSON.parse(readFileSync("./data.json", "utf8"));
export default { answer: data.answer, file: basename("/tmp/data.json") };
"#,
    )
    .unwrap();

    let result = e_agent_node_runtime::run_file(dir.path().join("main.ts"))
        .await
        .unwrap();

    assert_eq!(
        result,
        serde_json::json!({ "answer": 42.0, "file": "data.json" })
    );
}
