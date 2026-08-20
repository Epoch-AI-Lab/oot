fn login(user: &str) -> bool {
    !user.is_empty() && user != "root"
}

fn logout() {
    println!("bye");
}
