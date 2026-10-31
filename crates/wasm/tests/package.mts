import init, {
    build,
    generate,
    skill,
    spec,
    validate,
    write,
    type GenerateOptions,
    type HttpTransport,
    type WriteResult,
} from "../../../target/wasm-pkg/bts_wasm.js";

function assert(condition: unknown, message: string): asserts condition {
    if (!condition) throw new Error(message);
}

export async function check(bytes: Uint8Array<ArrayBuffer>): Promise<void> {
    await init({ module_or_path: bytes });

    const source = 'trace "run" { output = "hello" }';
    const validation = validate(source);
    assert(validation.valid && validation.diagnostics.length === 0, "valid source was rejected");
    const invalid = validate('trace "run" { output = var.missing }');
    assert(!invalid.valid && invalid.diagnostics[0]?.phase === "validation", "missing semantic diagnostic");
    assert(typeof spec() === "object" && spec() !== null, "missing language reference");
    assert(skill().includes("generate(source, options)"), "missing browser guidance");

    const options: GenerateOptions = {
        count: 2,
        over_ms: 1000,
        until_ms: 5000,
        seed: "42",
        id_prefix: "package-check",
    };
    const generated = generate(source, options);
    if (!generated.ok) throw new Error(`${generated.error.stage}: ${generated.error.message}`);
    assert(generated.value.provider === "braintrust", "unexpected generation provider");
    assert(generated.value.event_count === 2 && generated.value.payload.events.length === 2, "missing events");
    assert(generated.value.payload.events.every(event => event.output === "hello"), "unexpected event output");

    const rejected = generate('trace "run" { output = var.missing }', options);
    if (rejected.ok) throw new Error("invalid source generated successfully");
    assert(rejected.error.stage === "compile" && rejected.error.diagnostics.length > 0, "missing operation error");

    const built = build('scorer "quality" { code { score = 0.5 } }', {
        project: "Package Check",
        lang: "typescript",
    });
    if (!built.ok) throw new Error(`${built.error.stage}: ${built.error.message}`);
    assert(built.value.files.some(file => file.path.endsWith(".ts") && file.contents.length > 0), "missing scorer file");

    let requests = 0;
    const transport: HttpTransport = async request => {
        requests += 1;
        assert(request.method === "POST", "unexpected HTTP method");
        assert(request.path === "/v1/project_logs/00000000-0000-0000-0000-000000000000/insert", "unexpected provider path");
        assert(request.headers["content-type"] === "application/json", "missing content type");
        assert(request.body instanceof Uint8Array && request.signal instanceof AbortSignal, "invalid transport values");
        const payload: typeof generated.value.payload = JSON.parse(new TextDecoder().decode(request.body));
        assert(payload.events.length === generated.value.payload.events.length, "write event count differs from preview");
        assert(payload.events.every((event, index) =>
            event.id === generated.value.payload.events[index].id && event.output === "hello"
        ), "write events differ from preview");
        await Promise.resolve();
        return { status: 200, body: new TextEncoder().encode('{"row_ids":["row-1","row-2"]}') };
    };
    const pending: Promise<WriteResult> = write(source, options, {
        project_id: "00000000-0000-0000-0000-000000000000",
    }, transport);
    assert(pending instanceof Promise, "write did not return a Promise");
    const written = await pending;
    if (!written.ok) throw new Error(`${written.error.stage}: ${written.error.message}`);
    assert(requests === 1 && written.value.row_ids.join(",") === "row-1,row-2", "missing acknowledged rows");
    assert(written.value.event_count === generated.value.event_count, "incorrect write event count");
    console.log("browser package TypeScript check passed");
}
