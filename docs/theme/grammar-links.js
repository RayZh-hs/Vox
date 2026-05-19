(function () {
    const BLOCK_SELECTOR = "pre > code.language-ebnf";
    const DEFINITION_RE = /^[A-Za-z][A-Za-z0-9]*$/;

    function escapeHtml(text) {
        return text
            .replaceAll("&", "&amp;")
            .replaceAll("<", "&lt;")
            .replaceAll(">", "&gt;")
            .replaceAll('"', "&quot;");
    }

    function isIdentifierStart(char) {
        return /[A-Za-z]/.test(char);
    }

    function isIdentifierContinue(char) {
        return /[A-Za-z0-9]/.test(char);
    }

    function makeAnchorId(name, occurrence) {
        const base = `grammar-${name.toLowerCase()}`;
        return occurrence === 1 ? base : `${base}-${occurrence}`;
    }

    function parseDefinitions(lines) {
        const definitions = [];

        for (let index = 0; index < lines.length - 1; index += 1) {
            const name = lines[index].trim();

            if (DEFINITION_RE.test(name) && /^\s*::=/.test(lines[index + 1])) {
                definitions.push({ lineIndex: index, name });
            }
        }

        return definitions;
    }

    function collectBlocks(root) {
        return Array.from(root.querySelectorAll(BLOCK_SELECTOR)).map((element) => {
            const lines = (element.textContent || "").replace(/\n$/, "").split(/\r?\n/);
            return {
                element,
                lines,
                definitions: parseDefinitions(lines),
            };
        });
    }

    function renderLine(line, lineIndex, definitionsByLine, definitionsByName, currentPage) {
        const definition = definitionsByLine.get(lineIndex);
        let html = "";
        let offset = 0;

        if (definition) {
            const leading = line.match(/^\s*/)?.[0] || "";
            html += escapeHtml(leading);
            html += `<a class="grammar-symbol grammar-definition" id="${definition.id}" href="#${definition.id}">${escapeHtml(definition.name)}</a>`;
            offset = leading.length + definition.name.length;
        }

        while (offset < line.length) {
            const current = line[offset];

            if (current === '"') {
                let end = offset + 1;

                while (end < line.length) {
                    if (line[end] === '"' && line[end - 1] !== "\\") {
                        end += 1;
                        break;
                    }

                    end += 1;
                }

                html += escapeHtml(line.slice(offset, end));
                offset = end;
                continue;
            }

            if (current === "<") {
                const end = line.indexOf(">", offset);
                const stop = end === -1 ? line.length : end + 1;
                html += escapeHtml(line.slice(offset, stop));
                offset = stop;
                continue;
            }

            if (isIdentifierStart(current)) {
                let end = offset + 1;

                while (end < line.length && isIdentifierContinue(line[end])) {
                    end += 1;
                }

                const token = line.slice(offset, end);
                const target = definitionsByName.get(token);

                if (target) {
                    const href =
                        target.pathname === currentPage.pathname && target.search === currentPage.search
                            ? `#${target.id}`
                            : `${target.pathname}${target.search}#${target.id}`;

                    html += `<a class="grammar-symbol grammar-reference" href="${href}">${escapeHtml(token)}</a>`;
                } else {
                    html += escapeHtml(token);
                }

                offset = end;
                continue;
            }

            html += escapeHtml(current);
            offset += 1;
        }

        return html;
    }

    function renderBlocks(blocks, definitionsByName, currentPage) {
        for (const block of blocks) {
            const definitionsByLine = new Map(block.definitions.map((definition) => [definition.lineIndex, definition]));
            const html = block.lines
                .map((line, index) => renderLine(line, index, definitionsByLine, definitionsByName, currentPage))
                .join("\n");

            block.element.innerHTML = html;
        }
    }

    function ensureHashTarget() {
        const hash = window.location.hash.slice(1);

        if (!hash.startsWith("grammar-")) {
            return;
        }

        const target = document.getElementById(hash);

        if (!target) {
            return;
        }

        requestAnimationFrame(() => {
            target.scrollIntoView({ block: "center" });
        });
    }

    function uniquePageUrls() {
        const urls = new Map();

        for (const link of document.querySelectorAll("a[href]")) {
            const href = link.getAttribute("href");

            if (!href || href.startsWith("#")) {
                continue;
            }

            let url;

            try {
                url = new URL(href, window.location.href);
            } catch {
                continue;
            }

            if (url.origin !== window.location.origin || !url.pathname.endsWith(".html")) {
                continue;
            }

            urls.set(url.pathname, url);
        }

        return Array.from(urls.values());
    }

    async function loadDefinitionIndex(definitionsByName, currentPage) {
        const pages = uniquePageUrls().filter(
            (page) => !(page.pathname === currentPage.pathname && page.search === currentPage.search),
        );

        for (const page of pages) {
            try {
                const response = await fetch(page.href);

                if (!response.ok) {
                    continue;
                }

                const html = await response.text();
                const documentFragment = new DOMParser().parseFromString(html, "text/html");
                const blocks = collectBlocks(documentFragment);
                const counts = new Map();

                for (const block of blocks) {
                    for (const definition of block.definitions) {
                        const occurrence = (counts.get(definition.name) || 0) + 1;
                        counts.set(definition.name, occurrence);

                        if (!definitionsByName.has(definition.name)) {
                            definitionsByName.set(definition.name, {
                                id: makeAnchorId(definition.name, occurrence),
                                pathname: page.pathname,
                                search: page.search,
                            });
                        }
                    }
                }
            } catch {
                continue;
            }
        }
    }

    async function main() {
        const blocks = collectBlocks(document);

        if (blocks.length === 0) {
            return;
        }

        const currentPage = new URL(window.location.href);
        const definitionsByName = new Map();
        const counts = new Map();

        for (const block of blocks) {
            for (const definition of block.definitions) {
                const occurrence = (counts.get(definition.name) || 0) + 1;
                counts.set(definition.name, occurrence);

                definition.id = makeAnchorId(definition.name, occurrence);

                if (!definitionsByName.has(definition.name)) {
                    definitionsByName.set(definition.name, {
                        id: definition.id,
                        pathname: currentPage.pathname,
                        search: currentPage.search,
                    });
                }
            }
        }

        renderBlocks(blocks, definitionsByName, currentPage);
        ensureHashTarget();

        await loadDefinitionIndex(definitionsByName, currentPage);
        renderBlocks(blocks, definitionsByName, currentPage);
        ensureHashTarget();
    }

    window.addEventListener("DOMContentLoaded", () => {
        void main();
    });
})();
