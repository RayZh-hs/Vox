# Vox Documentation

Vox is a high-performance, data-flow compatible programming language, designed with modern syntax, optimized for vector processing and parallel execution.

## Why Vox?

Many programs come with embedded expressions and node graphs to provide users with simple program-level control. Examples include Excel, Houdini, Blender Geometry Nodes, and Unreal Engine's Blueprints. Traditionally for users to do the scripting each program has its own scripting language, and in recent years they seem to be converging on standard scripting languages like Python and Lua.

Python and Lua are great languages, but they both have issues. While Python is concise and modern, its flexibility comes at the cost of performance. Lua is fast, but its syntax is, in my humble opinion, not as user-friendly and far from modern.

From another perspective, both languages are designed for general-purpose programming, and they fail to capture the unique needs of node-based programming (NBP). In NBP, live previewing is often required, and caching internal states is common. Traditionally this is achieved either by interpreting the code and user-managing state info, or by re-compiling every time the code changes. Both are slow and inefficient. Additionally we can find that self-recursion, unbounded loops and side effects common in general-purpose programming are often not present. This prompts us to redesign the language, capturing the unique needs of NBP, and optimizing for it.

Thus Vox is born, a language designed for NBP scripting and execution. It features a modern syntax, optimized performance, and superior support node-based programming. With Vox, users can enjoy a seamless scripting experience while benefiting from the efficiency and capabilities tailored for node-based workflows.

## Getting Started

Vox is currently in early development, and to install the development version you need to manually clone the repository and build it from source.

```
git clone https://github.com/RayZh-hs/Vox
cd Vox
```

Use cargo to build the project and spawn the REPL:

```
cargo run repl
```
