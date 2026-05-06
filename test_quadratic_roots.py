import sys


def is_root(a: float, b: float, c: float, x: float, tol: float = 1e-9) -> bool:
    """Check whether x satisfies a*x^2 + b*x + c == 0."""
    return abs(a * x * x + b * x + c) <= tol


def is_cubic_root(
    a: float,
    b: float,
    c: float,
    d: float,
    x: float,
    tol: float = 1e-9,
) -> bool:
    """Check whether x satisfies a*x^3 + b*x^2 + c*x + d == 0."""
    return abs(a * x**3 + b * x**2 + c * x + d) <= tol


def root_message(first: bool, second: bool, equation: str) -> str:
    if first and second:
        return f"Both numbers are roots of the {equation} equation."
    if first:
        return f"Only the first number is a root of the {equation} equation."
    if second:
        return f"Only the second number is a root of the {equation} equation."
    return f"Neither number is a root of the {equation} equation."


def parse_numbers(args: list[str]) -> list[float]:
    try:
        return [float(arg) for arg in args]
    except ValueError as exc:
        raise ValueError("All arguments must be numbers.") from exc


def check_roots(args: list[str]) -> str:
    if len(args) not in (5, 6):
        raise ValueError("Quadratic expects 5 numbers, cubic expects 6 numbers.")

    numbers = parse_numbers(args)
    if len(numbers) == 5:
        a, b, c, r1, r2 = numbers
        if a == 0:
            raise ValueError("Coefficient 'a' must be non-zero for a quadratic equation.")
        return root_message(is_root(a, b, c, r1), is_root(a, b, c, r2), "quadratic")

    a, b, c, d, r1, r2 = numbers
    if a == 0:
        raise ValueError("Coefficient 'a' must be non-zero for a cubic equation.")
    return root_message(is_cubic_root(a, b, c, d, r1), is_cubic_root(a, b, c, d, r2), "cubic")


def main() -> int:
    try:
        print(check_roots(sys.argv[1:]))
    except ValueError as exc:
        print(exc)
        return 1
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
