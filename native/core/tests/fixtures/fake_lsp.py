#!/usr/bin/env python3
"""A tiny language server for tests.

Every line containing ``ERROR:<word>`` gets an error diagnostic "bad <word>",
every ``WARN:<word>`` a warning. Definition requests answer the first line
that contains ``def <identifier under the cursor>``; references answer every
line that mentions it. Flags:

  --crash-on-open    exit(3) as soon as a document is opened
  --delay-ms N       wait N ms before publishing diagnostics
  --log PATH         append each received method name to PATH
  --stop-reading     stop reading input after initialization (a hung server)
"""
import json
import re
import sys
import time

ARGS = sys.argv[1:]
CRASH = "--crash-on-open" in ARGS
DELAY = int(ARGS[ARGS.index("--delay-ms") + 1]) / 1000 if "--delay-ms" in ARGS else 0
LOG = ARGS[ARGS.index("--log") + 1] if "--log" in ARGS else None
STOP_READING = "--stop-reading" in ARGS

docs = {}
config_answer = None
next_id = 1000


def read():
    length = None
    while True:
        line = sys.stdin.buffer.readline()
        if not line:
            return None
        line = line.decode().strip()
        if not line:
            break
        key, _, value = line.partition(":")
        if key.lower() == "content-length":
            length = int(value.strip())
    return json.loads(sys.stdin.buffer.read(length))


def send(message):
    body = json.dumps(message).encode()
    sys.stdout.buffer.write(b"Content-Length: %d\r\n\r\n" % len(body) + body)
    sys.stdout.buffer.flush()


def publish(uri):
    if DELAY:
        time.sleep(DELAY)
    text, version = docs[uri]
    diagnostics = []
    for number, line in enumerate(text.splitlines()):
        for severity, tag in ((1, "ERROR:"), (2, "WARN:")):
            for match in re.finditer(re.escape(tag) + r"(\w+)", line):
                diagnostics.append({
                    "range": {
                        "start": {"line": number, "character": match.start()},
                        "end": {"line": number, "character": match.end()},
                    },
                    "severity": severity,
                    "source": "fake",
                    "message": "bad " + match.group(1),
                })
    if "SHOWCONFIG" in text:
        diagnostics.append({
            "range": {"start": {"line": 0, "character": 0}, "end": {"line": 0, "character": 0}},
            "severity": 3,
            "message": "config " + json.dumps(config_answer, sort_keys=True),
        })
    send({"jsonrpc": "2.0", "method": "textDocument/publishDiagnostics",
          "params": {"uri": uri, "version": version, "diagnostics": diagnostics}})


def word_at(uri, position):
    lines = docs[uri][0].splitlines()
    line = lines[position["line"]] if position["line"] < len(lines) else ""
    for match in re.finditer(r"\w+", line):
        if match.start() <= position["character"] <= match.end():
            return match.group(0)
    return ""


def location(uri, number, column):
    return {"uri": uri, "range": {"start": {"line": number, "character": column},
                                  "end": {"line": number, "character": column}}}


while True:
    message = read()
    if message is None:
        break
    method = message.get("method")
    if LOG and method:
        with open(LOG, "a") as log:
            log.write(method + "\n")
    if method is None:
        # A response to our own request.
        if message.get("id") == 999:
            config_answer = message.get("result")
        continue
    params = message.get("params") or {}
    if method == "initialize":
        send({"jsonrpc": "2.0", "id": message["id"], "result": {"capabilities": {
            "textDocumentSync": 1, "definitionProvider": True, "referencesProvider": True}}})
    elif method == "initialized":
        send({"jsonrpc": "2.0", "id": 999, "method": "workspace/configuration",
              "params": {"items": [{"section": "python.analysis"}]}})
        if STOP_READING:
            while True:
                time.sleep(60)
    elif method == "textDocument/didOpen":
        if CRASH:
            sys.exit(3)
        doc = params["textDocument"]
        docs[doc["uri"]] = (doc["text"], doc["version"])
        publish(doc["uri"])
    elif method == "textDocument/didChange":
        doc = params["textDocument"]
        docs[doc["uri"]] = (params["contentChanges"][-1]["text"], doc["version"])
        publish(doc["uri"])
    elif method == "textDocument/definition":
        uri = params["textDocument"]["uri"]
        name = word_at(uri, params["position"])
        result = None
        for other, (text, _) in docs.items():
            for number, line in enumerate(text.splitlines()):
                column = line.find("def " + name)
                if name and column >= 0:
                    result = [location(other, number, column + 4)]
                    break
            if result:
                break
        send({"jsonrpc": "2.0", "id": message["id"], "result": result})
    elif method == "textDocument/references":
        uri = params["textDocument"]["uri"]
        name = word_at(uri, params["position"])
        found = []
        for other, (text, _) in docs.items():
            for number, line in enumerate(text.splitlines()):
                for match in re.finditer(r"\b%s\b" % re.escape(name), line):
                    found.append(location(other, number, match.start()))
        send({"jsonrpc": "2.0", "id": message["id"], "result": found})
    elif method == "shutdown":
        send({"jsonrpc": "2.0", "id": message["id"], "result": None})
    elif method == "exit":
        sys.exit(0)
    elif "id" in message:
        send({"jsonrpc": "2.0", "id": message["id"], "error": {"code": -32601, "message": "nope"}})
