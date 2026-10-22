//! A small sample database for first-run exploration.

pub const SAMPLE_SQL: &str = "
CREATE TABLE Customers (
  CustomerID COUNTER PRIMARY KEY,
  Company TEXT NOT NULL,
  City TEXT,
  Country TEXT,
  Active YESNO DEFAULT Yes
);
CREATE TABLE Products (
  ProductID COUNTER PRIMARY KEY,
  ProductName TEXT NOT NULL,
  UnitPrice CURRENCY,
  InStock INTEGER DEFAULT 0
);
CREATE TABLE Orders (
  OrderID COUNTER PRIMARY KEY,
  CustomerID INTEGER REFERENCES Customers (CustomerID),
  OrderDate DATETIME,
  Freight CURRENCY
);
CREATE TABLE OrderDetails (
  DetailID COUNTER PRIMARY KEY,
  OrderID INTEGER REFERENCES Orders (OrderID) ON DELETE CASCADE,
  ProductID INTEGER REFERENCES Products (ProductID),
  Quantity INTEGER
);

INSERT INTO Customers (Company, City, Country) VALUES
  ('Alfreds Futterkiste', 'Berlin', 'Germany'),
  ('Ana Trujillo Emparedados', 'Mexico D.F.', 'Mexico'),
  ('Around the Horn', 'London', 'UK'),
  ('Berglunds snabbkop', 'Lulea', 'Sweden'),
  ('Blondel pere et fils', 'Strasbourg', 'France'),
  ('Cactus Comidas', 'Buenos Aires', 'Argentina'),
  ('Drachenblut Delikatessen', 'Aachen', 'Germany'),
  ('Eastern Connection', 'London', 'UK');
INSERT INTO Products (ProductName, UnitPrice, InStock) VALUES
  ('Chai', 18, 39), ('Chang', 19, 17), ('Aniseed Syrup', 10, 13), ('Chef Anton''s Gumbo Mix', 21.35, 0),
  ('Grandma''s Boysenberry Spread', 25, 120), ('Uncle Bob''s Dried Pears', 30, 15), ('Northwoods Cranberry Sauce', 40, 6);
INSERT INTO Orders (CustomerID, OrderDate, Freight) VALUES
  (1, #2024-01-15#, 32.38), (2, #2024-01-18#, 11.61), (3, #2024-02-02#, 65.83), (1, #2024-02-20#, 41.34),
  (3, #2024-03-05#, 51.30), (5, #2024-03-11#, 58.17), (8, #2024-04-01#, 22.98), (3, #2024-04-19#, 148.33);
INSERT INTO OrderDetails (OrderID, ProductID, Quantity) VALUES
  (1, 1, 12), (1, 2, 10), (2, 3, 5), (3, 5, 9), (3, 6, 2), (4, 1, 20), (5, 7, 4), (6, 2, 15), (7, 4, 6), (8, 5, 30), (8, 7, 8);

CREATE VIEW [Order Totals] AS
  SELECT o.OrderID, c.Company, o.OrderDate, Sum(d.Quantity * p.UnitPrice) AS Subtotal
  FROM ((Orders o INNER JOIN Customers c ON c.CustomerID = o.CustomerID)
        INNER JOIN OrderDetails d ON d.OrderID = o.OrderID)
        INNER JOIN Products p ON p.ProductID = d.ProductID
  GROUP BY o.OrderID, c.Company, o.OrderDate;
CREATE VIEW [Sales by Country] AS
  SELECT c.Country, Count(DISTINCT o.OrderID) AS Orders, Sum(d.Quantity * p.UnitPrice) AS Revenue
  FROM ((Customers c INNER JOIN Orders o ON c.CustomerID = o.CustomerID)
        INNER JOIN OrderDetails d ON d.OrderID = o.OrderID)
        INNER JOIN Products p ON p.ProductID = d.ProductID
  GROUP BY c.Country;
";

#[cfg(test)]
mod tests {
    #[test]
    fn sample_loads_and_enforces_relationships() {
        let mut db = accessor_core::Database::in_memory();
        db.execute(super::SAMPLE_SQL).unwrap();
        assert!(db.execute("INSERT INTO Orders (CustomerID) VALUES (999)").is_err());
        assert!(db.execute("DELETE FROM Customers WHERE CustomerID = 1").is_err());
        db.execute("DELETE FROM Orders WHERE OrderID = 1").unwrap();
        let left = db.query("SELECT Count(*) FROM OrderDetails WHERE OrderID = 1").unwrap();
        assert_eq!(left.rows[0][0].to_string(), "0");
    }
}
