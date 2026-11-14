---
id: "01a1d7bf-c723-766c-bc26-b2cddcb463f8"
key: "REQ-3"
grounds: ["F-2"]
title: "Guarding hookwatch's state stops writes, not reads"
---
tamper-guard blocks writes to hook settings and to hookwatch's state, by file tools and by shell commands that look like writes. Reads, by the Read tool or a read-only shell command, pass.
