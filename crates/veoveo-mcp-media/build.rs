//! Embeds the skills under `skills/`, which the server serves at `skill://media/…`.
fn main() {
    veoveo_skills::build::embed_skills("skills").expect("Media's skills");
}
