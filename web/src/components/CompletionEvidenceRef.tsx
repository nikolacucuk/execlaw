import { ArtifactDownloadLink } from "./ArtifactDownloadLink";

/** Render a recognized evidence reference as a useful link and unknown values as inert text. */
export function CompletionEvidenceRef({ reference }: { reference: string }) {
    const attachment = /^attachment:([A-Za-z0-9_-]{1,128})$/.exec(reference);
    if (attachment) {
        return <ArtifactDownloadLink artifactId={attachment[1]} />;
    }
    const trace = /^trace:(\d{1,20})$/.exec(reference);
    if (trace) {
        return <a href={`#trace-${trace[1]}`}>Trace event {trace[1]}</a>;
    }
    const step = /^run:[A-Za-z0-9:_-]+\/step:([A-Za-z0-9:_-]+)$/.exec(reference);
    if (step) {
        return <a href={`#step-${encodeURIComponent(step[1])}`}>Run step {step[1]}</a>;
    }
    const agentOutput = /^agent-run:([A-Za-z0-9_-]+)\/output$/.exec(reference);
    if (agentOutput) {
        return <a href={`#agent-run-${agentOutput[1]}`}>Agent output</a>;
    }
    const attestation = /^attestation:(\d{1,20})$/.exec(reference);
    if (attestation) {
        return <a href={`/settings/audit?entry=${attestation[1]}`}>Controller attestation {attestation[1]}</a>;
    }
    if (/^https:\/\//i.test(reference)) {
        return <a href={reference} target="_blank" rel="noreferrer noopener">{reference}</a>;
    }
    return <code>{reference}</code>;
}
