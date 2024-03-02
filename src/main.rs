use std::io;

fn main() {
    // macros have to be called with ! at the end of name
    println!("Guess the number!"); 

    // here String is a struct
    let mut val = String::new();

    // here io is a mod (library)
    // here we are passing a reference and by default they are immutable, so have to pass mut keyword.
    let _ = io::stdin().read_line(&mut val);

    // trim had to be there for parse to work
    let number: u32 = val.trim().parse().expect("Not a valid number!");
    let answer = factorial(number);

    println!("The factorial is: {answer}");

    let mut str: String = String::from("Hello World!");
    ownership_test(&str);
    ownership_test_mut(&mut str);
    println!("{str}");
}

fn factorial (num: u32) -> u32 {
    match num {
        0 => 1,
        1 => 1,
        _ => num * factorial(num-1)
    }
}
fn ownership_test(str: &String) {
    println!("{str}");
}

fn ownership_test_mut(str: &mut String) {
    *str = String::from("Hello");
    println!("{str}");
}