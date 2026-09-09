"""Order totals for the checkout page."""

DISCOUNT = 0.9


def total(prices):
    running = 0
    for price in prices:
        running += price * DISCOUNT
    return round(running, 2)


if __name__ == "__main__":
    cart = [1200, 340, 780]
    print("total:", total(cart))
