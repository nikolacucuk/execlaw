import { useState, type PointerEvent } from "react";
import preview from "../generated/graphifyPreview.json";

interface PreviewNode {
    id: string;
    label: string;
    community: number;
}

interface PositionedNode extends PreviewNode {
    x: number;
    y: number;
}

const WIDTH = 820;
const HEIGHT = 196;
const MAX_NODES = 72;

function hash(value: string, seed: number): number {
    let result = seed;
    for (let index = 0; index < value.length; index += 1) {
        result = Math.imul(result ^ value.charCodeAt(index), 16777619);
    }
    return result >>> 0;
}

const nodes: PositionedNode[] = (preview.nodes as PreviewNode[])
    .slice(0, MAX_NODES)
    .map((node) => ({
        ...node,
        x: 18 + (hash(node.id, 2166136261) % (WIDTH - 36)),
        y: 14 + (hash(node.id, 2246822519) % (HEIGHT - 28)),
    }));

const links = nodes.flatMap((node, index) => {
    const peer = nodes
        .slice(index + 1)
        .find((candidate) => candidate.community === node.community);
    return peer ? [{ source: node, target: peer }] : [];
});

export function GraphifyPreview() {
    const [focused, setFocused] = useState<PositionedNode | null>(null);

    const focusNearest = (event: PointerEvent<SVGSVGElement>) => {
        const bounds = event.currentTarget.getBoundingClientRect();
        const x = ((event.clientX - bounds.left) / bounds.width) * WIDTH;
        const y = ((event.clientY - bounds.top) / bounds.height) * HEIGHT;
        let nearest: PositionedNode | null = null;
        let nearestDistance = Number.POSITIVE_INFINITY;
        for (const node of nodes) {
            const distance = Math.hypot(node.x - x, node.y - y);
            if (distance < nearestDistance) {
                nearest = node;
                nearestDistance = distance;
            }
        }
        setFocused(nearestDistance <= 72 ? nearest : null);
    };

    return (
        <section
            className="execlaw-graphify-preview"
            data-testid="graphify-preview"
            aria-label="Repository knowledge graph preview"
        >
            <div className="execlaw-graphify-preview__head">
                <span className="execlaw-graphify-preview__title">
                    Repository graph
                </span>
                <span className="execlaw-graphify-preview__meta">
                    {nodes.length} nodes · {links.length} local links
                </span>
            </div>
            <svg
                className="execlaw-graphify-preview__canvas"
                viewBox={`0 0 ${WIDTH} ${HEIGHT}`}
                role="img"
                aria-label="Interactive sample of the local Graphify graph"
                onPointerMove={focusNearest}
                onPointerLeave={() => setFocused(null)}
            >
                <g className="execlaw-graphify-preview__links">
                    {links.map(({ source, target }) => (
                        <line
                            key={`${source.id}:${target.id}`}
                            x1={source.x}
                            y1={source.y}
                            x2={target.x}
                            y2={target.y}
                        />
                    ))}
                </g>
                <g className="execlaw-graphify-preview__nodes">
                    {nodes.map((node, index) => (
                        <circle
                            key={node.id}
                            cx={node.x}
                            cy={node.y}
                            r={focused?.id === node.id ? 5 : 2.5}
                            className={focused?.id === node.id ? "is-focused" : undefined}
                            style={{ animationDelay: `${index * 24}ms` }}
                        />
                    ))}
                </g>
            </svg>
            <div className="execlaw-graphify-preview__hint">
                {focused
                    ? `${focused.label} · community ${focused.community}`
                    : "Move across the graph to inspect nearby repository nodes"}
            </div>
        </section>
    );
}
