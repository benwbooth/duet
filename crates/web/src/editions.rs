//! "Find another edition" on a book's page: a card that opens Discover already searching for
//! this book, where any other ebook or audiobook of it can be fetched.

use leptos::prelude::*;

use crate::{icon, icons};

#[component]
pub fn FindEdition(title: String) -> impl IntoView {
    let href = format!("#/discover/{}", js_sys::encode_uri_component(&title));
    view! {
        <a class="edition-card find-edition" href=href>
            <div class="action-icon">{icon(icons::DISCOVER)}</div>
            <div>
                <strong>"Find another edition"</strong>
                <p>"Search Discover for a different ebook or audiobook of this book"</p>
            </div>
        </a>
    }
}
