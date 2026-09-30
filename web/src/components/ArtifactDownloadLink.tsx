import { useEffect, useState } from "react";
import type { ReactNode } from "react";
import { useAuth } from "../auth/AuthContext";
import { signDownloadUrl } from "../api/signedDownloadUrl";

/** A scoped, short-lived download link for an attachment-backed run artifact. */
export function ArtifactDownloadLink({ artifactId, children }: { artifactId: string; children?: ReactNode }) {
    const { getAccessToken } = useAuth();
    const [url, setUrl] = useState<string | null>(null);
    const [error, setError] = useState(false);

    useEffect(() => {
        let active = true;
        setUrl(null);
        setError(false);
        void signDownloadUrl(`/api/attachments/${encodeURIComponent(artifactId)}`, getAccessToken)
            .then((value) => { if (active) setUrl(value); })
            .catch(() => { if (active) setError(true); });
        return () => { active = false; };
    }, [artifactId, getAccessToken]);

    if (error) return <span className="small text-muted">Artifact unavailable</span>;
    if (!url) return <span className="small text-muted">Preparing artifact link…</span>;
    return <a className="small" href={url} download>{children ?? "Download result artifact"}</a>;
}
