use x_macro::select;

fn main() {
    let q = select! {
        select table.column
        from table
        where not table.column = null 
    };
    println!("{}", q);
}
